//! Request signing for the Web reader flow (content shards, reading
//! progress reports) — ported from koplugin's `lib/weread.lua`
//! (`WeRead.e`, `WeRead.sign`, `WeRead.sorted_query`,
//! `WeRead.urlencode`). Verified against real Lua output (see
//! docs/design.md's phase-3 notes) rather than the language-neutral
//! pseudocode in WeRead's own API reference doc — the pseudocode glossed
//! over a real quirk this module has to replicate: `r` is
//! `tostring(math.random(0,9999) ^ 2)`, and Lua's `^` always returns a
//! float, so real requests send `r` with a trailing `.0` (e.g.
//! `"1522756.0"`), not a clean integer string.
//!
//! None of this is real cryptography — `sign` is a bespoke, keyless,
//! reversible-by-construction hash WeRead's frontend uses to make
//! requests non-trivial to forge by hand, not to keep anything secret.

use md5::{Digest, Md5};

/// Ported from `WeRead.e` — a deterministic, MD5-based obfuscation of a
/// book/chapter/timestamp value, used as `b`/`c`/`pc` in content
/// requests and embedded in reader URLs (`bookHash`/`chapterHash`).
pub fn e_hash(value: &str) -> String {
    let h = format!("{:x}", Md5::digest(value.as_bytes()));
    let mut result = h[..3].to_string();

    let is_digit_string = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    let (type_flag, chunks): (char, Vec<String>) = if is_digit_string {
        // 9-digit chunks, each parsed as decimal then re-emitted as bare
        // hex (no zero-padding) — matches `string.format("%x", tonumber(part))`.
        let chunks = value
            .as_bytes()
            .chunks(9)
            .map(|c| {
                let n: u64 = std::str::from_utf8(c).unwrap().parse().unwrap_or(0);
                format!("{n:x}")
            })
            .collect();
        ('3', chunks)
    } else {
        // Each byte's value as bare (unpadded) hex, concatenated — matches
        // `string.format("%x", value:byte(i))` with no width specifier.
        let hex: String = value.bytes().map(|b| format!("{b:x}")).collect();
        ('4', vec![hex])
    };

    result.push(type_flag);
    result.push('2');
    result.push_str(&h[h.len() - 2..]);

    for (i, chunk) in chunks.iter().enumerate() {
        result.push_str(&format!("{:02x}", chunk.len()));
        result.push_str(chunk);
        if i + 1 < chunks.len() {
            result.push('g');
        }
    }

    if result.len() < 20 {
        result.push_str(&h[..20 - result.len()]);
    }

    let checksum = format!("{:x}", Md5::digest(result.as_bytes()));
    result.push_str(&checksum[..3]);
    result
}

/// JS `encodeURIComponent`-*like* percent-encoding — but narrower:
/// ported from `WeRead.urlencode`'s Lua pattern `[^%w%-_%.~]`, which
/// leaves only alnum/`-`/`_`/`.`/`~` unescaped. Real
/// `encodeURIComponent` also spares `!*'()`; this doesn't, and the
/// signature this feeds into is defined by WeRead's frontend doing
/// exactly this narrower version — matching a general-purpose
/// percent-encoder here would silently produce wrong signatures for any
/// value containing `!`, `*`, `'`, `(`, or `)`. `true`/`false`/`null`
/// spellings for bool/absent values mirror `js_string`.
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One request parameter, in the (small, fixed) set of JSON value shapes
/// content/progress requests actually use.
#[derive(Debug, Clone)]
pub enum Param {
    Str(String),
    Int(i64),
    Bool(bool),
}

impl Param {
    fn js_string(&self) -> String {
        match self {
            Param::Str(s) => s.clone(),
            Param::Int(n) => n.to_string(),
            Param::Bool(b) => b.to_string(), // "true" / "false", matches js_string
        }
    }
}

/// Builds the sorted `k=v&k2=v2...` query string signed fields are
/// derived from. Ported from `WeRead.sorted_query` — keys sorted
/// lexicographically, `s` itself excluded (it's what we're computing).
pub fn sorted_query(params: &[(&str, Param)]) -> String {
    let mut entries: Vec<&(&str, Param)> = params.iter().filter(|(k, _)| *k != "s").collect();
    entries.sort_by_key(|(k, _)| *k);
    entries
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(&v.js_string())))
        .collect::<Vec<_>>()
        .join("&")
}

/// Ported from `WeRead.sign`: a keyless, order-dependent XOR/shift hash
/// over the query string, walked from the end in pairs of bytes.
pub fn sign(query: &str) -> String {
    let bytes = query.as_bytes();
    let length = bytes.len() as i64;
    let (mut a, mut b): (i64, i64) = (0x15051505, 0x15051505);
    let mut i = length;
    while i > 1 {
        let byte_i = bytes[(i - 1) as usize] as i64; // Lua query:byte(i), 1-indexed
        let byte_im1 = bytes[(i - 2) as usize] as i64; // query:byte(i - 1)
        a = (a ^ (byte_i << ((length - i + 1) % 30))) & 0x7fffffff;
        b = (b ^ (byte_im1 << ((i - 1) % 30))) & 0x7fffffff;
        i -= 2;
    }
    format!("{:x}", a + b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ground truth from koplugin's own docs/weread-api-reference.md
    // examples AND cross-checked against real Lua output (see
    // docs/design.md's phase-3 notes).
    #[test]
    fn e_hash_matches_documented_examples() {
        assert_eq!(e_hash("43208843"), "c9c321c07293508bc9c79df");
        assert_eq!(e_hash("2"), "c81322c012c81e728d9d180");
        assert_eq!(e_hash("119"), "07e323f027707e1cd7dc674");
    }

    // Ground truth captured by running a native-bitop reimplementation
    // of WeRead.e/WeRead.sign in real Lua 5.5 (the `bit` library koplugin
    // uses is LuaJIT-only; native `~`/`&`/`<<` are semantically
    // equivalent for these non-negative <2^31 values, and this
    // reimplementation's e_hash output was itself checked against the
    // documented examples above before trusting its other output).
    #[test]
    fn e_hash_non_numeric_matches_real_lua() {
        assert_eq!(e_hash("some-book-title"), "4b342bd1e736f6d652d626f6f6b2d7469746c65dbc");
        assert_eq!(e_hash("907755"), "6f932ec05dd9eb6f96f14b9");
    }

    #[test]
    fn urlencode_matches_real_lua() {
        assert_eq!(urlencode("hello world!"), "hello%20world%21");
        assert_eq!(urlencode("a/b?c=d"), "a%2Fb%3Fc%3Dd");
    }

    #[test]
    fn sorted_query_and_sign_match_real_lua() {
        let params: Vec<(&str, Param)> = vec![
            ("b", Param::Str(e_hash("43208843"))),
            ("c", Param::Str(e_hash("119"))),
            // Lua: tostring(1234 ^ 2) — `^` always returns a float, so
            // this carries a literal ".0" suffix that a real server
            // apparently accepts (see module docs).
            ("r", Param::Str("1522756.0".to_string())),
            ("ct", Param::Str("1780666397".to_string())),
            ("ps", Param::Str("abcpsvts123".to_string())),
            ("pc", Param::Str(e_hash("1780666397"))),
            ("sc", Param::Int(1)),
            ("prevChapter", Param::Bool(false)),
            ("st", Param::Int(0)),
        ];
        let query = sorted_query(&params);
        assert_eq!(
            query,
            "b=c9c321c07293508bc9c79df&c=07e323f027707e1cd7dc674&ct=1780666397&\
             pc=970321e07a9d14cfg017116&prevChapter=false&ps=abcpsvts123&\
             r=1522756.0&sc=1&st=0"
        );
        assert_eq!(sign(&query), "c7dd5c72");
    }
}
