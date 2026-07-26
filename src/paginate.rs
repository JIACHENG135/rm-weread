//! Splits a chapter's plain text into screen-sized pages.
//!
//! Narrow on purpose (design.md §"不重复造轮子"): this only has to lay
//! out WeRead chapter prose on a fixed-size e-ink screen, not implement a
//! general reflow engine. No hyphenation, no justification, no bidi.
//!
//! It does consult font metrics, though it once didn't: the original
//! character-grid model gave every CJK glyph two columns and everything
//! else one, which is true of CJK prose and wrong for Latin — an English
//! book came out visibly ragged, letters squeezed and craters after the
//! punctuation. Widths now come from `metrics`, the same source pdfgen
//! and layout use, in 1/1000 em.
//!
//! Each page keeps the character range it covers, so a tap on screen can
//! be mapped back through `xhtml::Text::source_offset` to a raw-HTML
//! index and handed to WeRead's underline/review APIs (see xhtml.rs).

/// One laid-out page. `start`/`end` are character offsets into the plain
/// text (`xhtml::Text::text`), not the source XHTML — convert with
/// `Text::source_offset` when talking to WeRead's APIs.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub lines: Vec<String>,
    /// Absolute character offset (into the chapter plain text) where
    /// each line in `lines` begins — same length as `lines`. The PDF
    /// layout (layout.rs) needs per-line offsets to place underlines,
    /// not just the page-level range.
    pub line_starts: Vec<usize>,
    pub start: usize,
    pub end: usize,
}

/// Rough width of one character in "columns", where a CJK glyph is 2 and
/// a Latin one is 1 — the standard monospace-ish approximation, and close
/// enough for a fixed-width e-ink layout.
/// Advance width of `c`, in 1/1000 em — see `metrics`.
///
/// Kept as a thin alias so callers read naturally; the single source of
/// truth is the embedded font.
pub fn char_units(c: char) -> u32 {
    crate::metrics::advance(c)
}

/// True where a line may break *before* `c` without splitting a word.
/// CJK breaks anywhere; Latin only at spaces (handled by the caller
/// tracking the last space).
fn is_cjk(c: char) -> bool {
    crate::metrics::is_full_width(c)
}

/// Characters that must not start a line (CJK closing punctuation) —
/// without this, a line ending exactly before "，" pushes the comma to
/// the next line, which reads as broken in Chinese typesetting.
fn forbidden_at_line_start(c: char) -> bool {
    matches!(c, '，' | '。' | '、' | '；' | '：' | '？' | '！' | '”' | '’' | '》' | '）' | '】' | '」' | '』' | '·' | '…' | ',' | '.' | ';' | ':' | '?' | '!' | ')' | ']' | '}')
}

/// Wraps one paragraph into lines no wider than `width` (1/1000 em).
/// Returns each line's text plus the character offset (relative to the
/// paragraph's start) where it begins.
fn wrap_paragraph(paragraph: &str, width: u32) -> Vec<(String, usize)> {
    let chars: Vec<char> = paragraph.chars().collect();
    if chars.is_empty() {
        return vec![(String::new(), 0)];
    }

    let mut lines = Vec::new();
    let mut line_start = 0usize;
    let mut cursor = 0usize;
    let mut used = 0u32;
    // Where the current line could break at a space, if it has to.
    let mut last_space: Option<usize> = None;

    while cursor < chars.len() {
        let c = chars[cursor];
        let w = char_units(c);

        if used + w > width && cursor > line_start {
            // Prefer breaking at a space for Latin runs; CJK can break
            // right here. Never break *before* closing punctuation.
            // A space landing exactly on the boundary is itself the
            // break point — falling back to an *earlier* space there
            // would needlessly drop a word that actually fit.
            let mut break_at = match last_space {
                Some(space) if !is_cjk(c) && c != ' ' => space + 1,
                _ => cursor,
            };
            if break_at < chars.len() && forbidden_at_line_start(chars[break_at]) && break_at > line_start + 1 {
                break_at -= 1;
            }
            let text: String = chars[line_start..break_at].iter().collect();
            lines.push((text.trim_end().to_string(), line_start));
            // Don't carry the break's whitespace onto the next line —
            // this also makes the recorded offset point at the line's
            // first real character, which is what the annotation
            // offset mapping wants.
            let mut next_start = break_at;
            while next_start < chars.len() && chars[next_start] == ' ' {
                next_start += 1;
            }
            line_start = next_start;
            cursor = next_start;
            used = 0;
            last_space = None;
            continue;
        }

        if c == ' ' {
            last_space = Some(cursor);
        }
        used += w;
        cursor += 1;
    }

    if line_start < chars.len() {
        let text: String = chars[line_start..].iter().collect();
        lines.push((text.trim_end().to_string(), line_start));
    }
    lines
}

/// Lays `text` out into pages of `lines_per_page` lines, each no wider
/// than `width` (1/1000 em — see `metrics`).
pub fn paginate(text: &str, width: u32, lines_per_page: usize) -> Vec<Page> {
    if width == 0 || lines_per_page == 0 {
        return Vec::new();
    }

    // (line text, absolute char offset in `text`)
    let mut all_lines: Vec<(String, usize)> = Vec::new();
    let mut para_offset = 0usize;
    for paragraph in text.split('\n') {
        for (line, rel) in wrap_paragraph(paragraph, width) {
            all_lines.push((line, para_offset + rel));
        }
        para_offset += paragraph.chars().count() + 1; // +1 for the '\n'
    }

    let total_chars = text.chars().count();
    let mut pages = Vec::new();
    for chunk in all_lines.chunks(lines_per_page) {
        pages.push(Page {
            lines: chunk.iter().map(|(l, _)| l.clone()).collect(),
            line_starts: chunk.iter().map(|(_, s)| *s).collect(),
            start: chunk[0].1,
            // Filled in below: a page ends where the next one starts, so
            // no character falls outside every page. The last page runs
            // to the end of the text.
            end: total_chars,
        });
    }
    for i in 0..pages.len().saturating_sub(1) {
        pages[i].end = pages[i + 1].start;
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_cjk_at_any_character() {
        // 3 em of text = 3 CJK chars per line
        let pages = paginate("一二三四五", 3000, 10);
        assert_eq!(pages[0].lines, vec!["一二三", "四五"]);
    }

    #[test]
    fn wraps_latin_at_spaces() {
        // Wide enough for "hello world" but not the next word — a
        // width in em now, so it is measured from the font.
        let width = crate::metrics::text_advance("hello world") + 100;
        let pages = paginate("hello world foo", width, 10);
        assert_eq!(pages[0].lines, vec!["hello world", "foo"]);
    }

    #[test]
    fn does_not_start_a_line_with_closing_punctuation() {
        // Without the rule, "，" would be pushed to the next line.
        let pages = paginate("一二三，四", 3000, 10);
        assert_eq!(pages[0].lines[0], "一二");
        assert!(pages[0].lines[1].starts_with("三，"));
    }

    #[test]
    fn splits_into_pages_of_requested_height() {
        let pages = paginate("一二三四五六七八九十", 1000, 2);
        // 1 CJK char per line, 2 lines per page => 5 pages
        assert_eq!(pages.len(), 5);
        assert_eq!(pages[0].lines, vec!["一", "二"]);
        assert_eq!(pages[4].lines, vec!["九", "十"]);
    }

    #[test]
    fn page_ranges_are_contiguous_and_cover_everything() {
        let text = "第一段文字\n第二段文字\n第三段文字";
        let pages = paginate(text, 2000, 2);
        assert_eq!(pages[0].start, 0);
        for w in pages.windows(2) {
            assert_eq!(w[0].end, w[1].start, "page ranges must be contiguous");
        }
        assert_eq!(pages.last().unwrap().end, text.chars().count());
    }

    #[test]
    fn paragraph_breaks_start_new_lines() {
        let pages = paginate("一\n二", 5000, 10);
        assert_eq!(pages[0].lines, vec!["一", "二"]);
    }

    #[test]
    fn empty_text_yields_one_empty_page() {
        let pages = paginate("", 5000, 10);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].lines, vec![""]);
    }

    #[test]
    fn zero_dimensions_yield_no_pages() {
        assert!(paginate("abc", 0, 10).is_empty());
        assert!(paginate("abc", 5000, 0).is_empty());
    }
}
