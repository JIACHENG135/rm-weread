//! Glyph advance widths from the embedded font, in 1/1000 em.
//!
//! Pagination, layout and PDF generation all have to agree, to the unit,
//! on how wide a character is: pagination decides where lines break,
//! layout turns character offsets into underline boxes, and pdfgen draws
//! the glyphs. Any disagreement shows up as underlines that sit slightly
//! off the words they belong to.
//!
//! They used to agree by *assumption* — a character grid where CJK was
//! two columns and everything else one. That is true for CJK prose and
//! badly wrong for Latin: in this font `A` is 608 units and `.` is 278,
//! and forcing both into a 500-unit cell squeezed the letters while
//! leaving craters after the punctuation. English books looked ragged.
//! Now all three ask the font.
//!
//! Units are 1/1000 em (PDF text-space units), and integers, so the sums
//! pagination accumulates are exact and reproducible — the frozen
//! geometry depends on the same input producing the same line breaks
//! forever.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use ttf_parser::{Face, GlyphId};

/// A full em. CJK glyphs are exactly this wide, which is why CJK prose
/// still lands on a tidy grid without anyone forcing it to.
pub const EM: u32 = 1000;

fn face() -> &'static Face<'static> {
    static FACE: OnceLock<Face<'static>> = OnceLock::new();
    FACE.get_or_init(|| Face::parse(crate::pdfgen::FONT, 0).expect("embedded font must parse"))
}

/// Advance width of `c` in 1/1000 em.
///
/// Characters the font has no glyph for fall back to a full em rather
/// than zero: an unknown character still occupies space on the page, and
/// a zero would let a line overflow instead of wrapping.
pub fn advance(c: char) -> u32 {
    static CACHE: OnceLock<RwLock<HashMap<char, u32>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(v) = cache.read().ok().and_then(|m| m.get(&c).copied()) {
        return v;
    }
    let f = face();
    let units_per_em = f.units_per_em() as f32;
    let v = f
        .glyph_index(c)
        .and_then(|g| f.glyph_hor_advance(GlyphId(g.0)))
        .map(|a| (a as f32 * EM as f32 / units_per_em).round() as u32)
        .filter(|a| *a > 0)
        .unwrap_or(EM);
    if let Ok(mut m) = cache.write() {
        m.insert(c, v);
    }
    v
}

/// Total advance of a run of text.
pub fn text_advance(s: &str) -> u32 {
    s.chars().map(advance).sum()
}

/// Advance of the first `char_idx` characters of `s` — i.e. how far into
/// the line the character at that index starts.
pub fn advance_before(s: &str, char_idx: usize) -> u32 {
    s.chars().take(char_idx).map(advance).sum()
}

/// Whether `c` is full-width, which is still how line breaking decides
/// it may break anywhere rather than only at spaces.
pub fn is_full_width(c: char) -> bool {
    advance(c) >= EM
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_is_a_full_em_and_latin_is_not() {
        for c in "一書、。，！？“”…".chars() {
            assert_eq!(advance(c), EM, "U+{:04X} {c}", c as u32);
            assert!(is_full_width(c));
        }
        for c in "aAiW,.".chars() {
            assert!(advance(c) < EM, "U+{:04X} {c} should be proportional", c as u32);
            assert!(!is_full_width(c));
        }
    }

    #[test]
    fn latin_widths_actually_differ_from_each_other() {
        // The whole point of the change: these used to be forced equal.
        assert_ne!(advance('i'), advance('W'));
        assert_ne!(advance('.'), advance('A'));
        assert!(advance('.') < advance('A'));
    }

    #[test]
    fn runs_sum_and_prefixes_line_up() {
        let s = "aA一。";
        assert_eq!(text_advance(s), s.chars().map(advance).sum::<u32>());
        assert_eq!(advance_before(s, 0), 0);
        assert_eq!(advance_before(s, 2), advance('a') + advance('A'));
        assert_eq!(advance_before(s, 99), text_advance(s));
    }

    #[test]
    fn an_unknown_glyph_still_takes_space() {
        // A zero-width fallback would let a line silently overflow.
        assert!(advance('\u{10FFFD}') > 0);
    }
}
