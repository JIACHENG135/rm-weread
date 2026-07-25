//! Chapter content decode — ported mechanically from koplugin's
//! `lib/content.lua` (`swap_positions`/`reverse_swaps`/`base64_decode`/
//! `decode_encoded_body`/`checked_body`/`decode_content_shards`). See
//! docs/design.md §"正文'解密'其实是三步可逆变换" for what this actually
//! is: no key, no real cryptography — a deterministic character-position
//! shuffle plus base64url, fully reversible from public, fixed logic.
//!
//! Ported against real Lua 5.5 output as ground truth (ran the original
//! functions verbatim on a handful of test strings, captured their
//! output, used it as this module's unit test expectations) rather than
//! trusting a read-through translation — this algorithm has enough
//! Lua-specific quirks (1-based indexing, `tonumber(bin_string, 4)`
//! reinterpreting a base-2 digit string as base-4) that "looks right" and
//! "is right" are not the same thing here.

use md5::{Digest, Md5};

const B64_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Verifies and strips the 32-char uppercase MD5 prefix each shard
/// response is wrapped in. Ported from `checked_body`.
pub fn checked_body(response_text: &str) -> Result<&str, String> {
    if response_text.len() <= 32 {
        return Ok("");
    }
    let (expected, body) = response_text.split_at(32);
    let actual = format!("{:X}", Md5::digest(body.as_bytes()));
    if actual != expected {
        return Err(format!("shard MD5 mismatch: expected {expected}, got {actual}"));
    }
    Ok(body)
}

/// Computes the swap-index list for `encoded` (already-stripped shard
/// body, no MD5 prefix). Ported byte-for-byte from `swap_positions`;
/// operates on bytes not chars, matching Lua string semantics — safe
/// here because the input is always ASCII (base64url-ish) at this stage.
fn swap_positions(encoded: &[u8]) -> Vec<usize> {
    let length = encoded.len();
    if length < 4 {
        return Vec::new();
    }
    if length < 11 {
        return vec![0, 2];
    }

    let n = 4.min(length.div_ceil(10)); // Lua: math.floor((length + 9) / 10)
    let mut tmp = String::new();
    // Lua: `for i = length, length - n + 1, -1` — the last n bytes,
    // walked from the very last byte backward.
    for i in (length - n..length).rev() {
        let mut byte = encoded[i] as u32;
        let mut bits = Vec::new();
        loop {
            bits.push(byte % 2);
            byte /= 2;
            if byte == 0 {
                break;
            }
        }
        bits.reverse();
        // `tonumber(bin_digit_string, 4)`: the string of 0/1 digits
        // reinterpreted as a base-4 number, not a base-2 one.
        let value = bits.iter().fold(0u64, |acc, &d| acc * 4 + d as u64);
        tmp.push_str(&value.to_string());
    }

    let tmp = tmp.as_bytes();
    let m = (length - n - 2) as i64;
    if m <= 0 {
        return Vec::new();
    }
    let step = m.to_string().len();
    let mut result = Vec::new();
    let mut i = 0usize;
    // Lua: `while ... i + step - 1 < #tmp` with 1-based `i` — translated
    // to 0-based `i`, that condition is `i + step < tmp.len()` (strict),
    // not `<=`. Got this wrong on the first pass (verified against real
    // Lua output for "UJDREVGR2hpams=": `<=` produced 8 positions where
    // Lua produces exactly 6).
    while result.len() < 10 && i + step < tmp.len() {
        let chunk: i64 = std::str::from_utf8(&tmp[i..i + step]).unwrap().parse().unwrap_or(0);
        result.push((chunk.rem_euclid(m)) as usize);
        let end2 = (i + step + 1).min(tmp.len());
        if i + 1 < tmp.len() {
            let chunk2: i64 =
                std::str::from_utf8(&tmp[i + 1..end2]).unwrap().parse().unwrap_or(0);
            result.push((chunk2.rem_euclid(m)) as usize);
        }
        i += step;
    }
    result
}

/// Applies the swap-index list to `encoded`, undoing WeRead's character
/// shuffle. Ported from `reverse_swaps`; positions come in as 0-based
/// offsets (matching `swap_positions`'s output), converted to Rust
/// 0-based string indices by dropping Lua's `+1`.
///
/// An odd-length `positions` list would index one past the start in the
/// original Lua (a latent bug there, never hit because real inputs
/// always produce an even count) — here it's just a no-op skip instead
/// of a crash, since we haven't seen a real case that exercises it.
fn reverse_swaps(encoded: &[u8], positions: &[usize]) -> Vec<u8> {
    let mut chars = encoded.to_vec();
    let mut i = positions.len();
    while i >= 2 {
        let (right, left) = (positions[i - 1], positions[i - 2]);
        for k in [1usize, 0] {
            let (li, ri) = (left + k, right + k);
            if li < chars.len() && ri < chars.len() {
                chars.swap(li, ri);
            }
        }
        i -= 2;
    }
    chars
}

/// Ported from `base64_decode` — **not** a standard/RFC base64 decoder,
/// on purpose: this first pass used the `base64` crate here and it
/// silently produced empty output on real inputs, because the Lua
/// original isn't RFC-conformant. It never validates padding length (a
/// body padded to 3 `=` — invalid per RFC, since a final group needs at
/// least 2 real chars — is accepted where a strict decoder rejects it
/// outright) and doesn't group input into 4-char blocks at all: it maps
/// every non-`=` char to its raw 6-bit index, concatenates *all* those
/// bits into one stream regardless of `=` boundaries, then slices that
/// stream into 8-bit bytes from the front, discarding whatever's left
/// over (0–7 bits) at the end. Ported as that bit-packing directly
/// instead, which is what actually matches real WeRead responses.
fn base64_decode(data: &[u8]) -> Vec<u8> {
    let normalized: Vec<u8> =
        data.iter().map(|&b| if b == b'-' { b'+' } else if b == b'_' { b'/' } else { b }).collect();

    let mut bits: Vec<u8> = Vec::with_capacity(normalized.len() * 6);
    for &b in &normalized {
        if b == b'=' {
            continue;
        }
        let Some(index) = B64_ALPHABET.iter().position(|&c| c == b) else { continue };
        for shift in (0..6).rev() {
            bits.push(((index >> shift) & 1) as u8);
        }
    }

    bits.chunks_exact(8)
        .map(|byte_bits| byte_bits.iter().fold(0u8, |acc, &bit| (acc << 1) | bit))
        .collect()
}

/// Ported from `decode_encoded_body`: drops the leading byte, undoes the
/// swap, then base64-decodes.
fn decode_encoded_body(body: &[u8]) -> Vec<u8> {
    if body.is_empty() {
        return Vec::new();
    }
    let encoded = &body[1..];
    let positions = swap_positions(encoded);
    let restored = reverse_swaps(encoded, &positions);
    base64_decode(&restored)
}

/// Decodes a full EPUB-format chapter (`e_0` + `e_1` + `e_3` shards, in
/// that order) or a TXT-format one (`t_0` + `t_1`, pass `e3 = ""`) into
/// raw XHTML/text bytes. Ported from `Content.decode_content_shards`.
pub fn decode_content_shards(e0: &str, e1: &str, e3: &str) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    body.extend_from_slice(checked_body(e0)?.as_bytes());
    body.extend_from_slice(checked_body(e1)?.as_bytes());
    body.extend_from_slice(checked_body(e3)?.as_bytes());
    Ok(decode_encoded_body(&body))
}

/// Decodes a single shard on its own (used for the `e_2` CSS shard).
/// Ported from `Content.decode_content_shard`.
pub fn decode_content_shard(shard: &str) -> Result<Vec<u8>, String> {
    Ok(decode_encoded_body(checked_body(shard)?.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    // Ground truth captured by running koplugin's *actual* Lua functions
    // (not a re-read of the source — the real interpreter) against these
    // exact input strings. See docs/design.md's phase-2 notes for how.
    #[test]
    fn swap_positions_matches_real_lua() {
        let cases: [(&str, &[usize]); 7] = [
            ("BCDEFGHIJKLMNOPQRSTUVWXYZ0123", &[12, 4, 13, 3, 12, 4, 12, 17, 12, 4]),
            ("UJDREVGR2hpams=", &[2, 3, 6, 4, 9, 5]),
            (
                "ello world this is a longer test string for shard",
                &[8, 13, 36, 22, 10, 38, 37, 5, 8, 12],
            ),
            // Exercises the `length < 4` branch (empty).
            ("bc", &[]),
            // Exercises the `length < 11` branch (fixed {0, 2}).
            ("bcdefgh", &[0, 2]),
            // A degenerate all-same-byte input and a longer mixed one,
            // to catch anything that only happens to work on "nice" text.
            (
                "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
                &[44, 42, 20, 4, 44, 42, 20, 4, 44, 42],
            ),
            ("omeMediumLengthTestString12345", &[12, 4, 22, 21, 12, 4, 21, 11, 12, 3]),
        ];
        for (input, expected) in cases {
            assert_eq!(swap_positions(input.as_bytes()), expected, "input={input:?}");
        }
    }

    #[test]
    fn decode_encoded_body_matches_real_lua() {
        let cases = [
            ("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123", "0420ce3441c824a2cc3d34904451945565d8674d76"),
            ("QUJDREVGR2hpams=", "509a43451121d9519a9a"),
            (
                "hello world this is a longer test string for shard",
                "7a59689e0addb289618a2b1a962c207abb5eb2db2dae89ed7e8aec85aadd",
            ),
            ("abc", "6d"),
            ("abcdefgh", "75e6dc7e08"),
            (
                "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
                "6596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596596",
            ),
            (
                "SomeMediumLengthTestString12345",
                "a267a0b5d8ae98b7ab89e8537acb52b679cc835db7e3",
            ),
        ];
        for (input, expected_hex) in cases {
            assert_eq!(hex(&decode_encoded_body(input.as_bytes())), expected_hex, "input={input:?}");
        }
    }

    #[test]
    fn checked_body_verifies_and_strips_md5_prefix() {
        let body = "hello world";
        let prefix = format!("{:X}", Md5::digest(body.as_bytes()));
        let wrapped = format!("{prefix}{body}");
        assert_eq!(checked_body(&wrapped).unwrap(), body);
    }

    #[test]
    fn checked_body_rejects_bad_md5() {
        let wrapped = format!("{}{}", "0".repeat(32), "hello world");
        assert!(checked_body(&wrapped).is_err());
    }

    #[test]
    fn checked_body_short_input_is_empty() {
        assert_eq!(checked_body("short").unwrap(), "");
    }
}
