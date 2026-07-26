//! Splits a chapter's plain text into screen-sized pages.
//!
//! Narrow on purpose (design.md §"不重复造轮子"): this only has to lay
//! out WeRead chapter prose on a fixed-size e-ink screen, not implement a
//! general reflow engine. No fonts, no metrics, no hyphenation — just a
//! character-grid model, which is what CJK prose on a fixed screen
//! actually is.
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
/// Grid columns a character occupies: 2 for full-width, 1 otherwise.
///
/// This must agree with what the embedded font actually draws, or the
/// PDF squeezes a full-width glyph into a half-width cell and the text
/// after it is dragged left. The ranges below are the CJK blocks, plus
/// the handful of "East Asian Ambiguous" punctuation marks that Noto
/// Sans CJK renders full-width even though their codepoints sit in the
/// Latin/General-Punctuation blocks — curly quotes were the ones that
/// showed up as visibly wrong on a real page.
///
/// Latin letters and ASCII punctuation deliberately do *not* match
/// their natural advances: the grid gives them a half-width cell and
/// pdfgen kerns them into it. That is the character-grid design, not a
/// mismatch. `pdfgen`'s `grid_widths_match_the_font` test encodes
/// exactly this distinction.
pub fn char_width(c: char) -> usize {
    let cp = c as u32;
    let wide = matches!(cp,
        0x1100..=0x115F      // Hangul Jamo
        | 0x2E80..=0xA4CF    // CJK radicals, kana, CJK ideographs
        | 0xAC00..=0xD7A3    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0xFE30..=0xFE6F    // CJK compatibility forms
        | 0xFF00..=0xFF60    // Fullwidth forms
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD  // CJK extensions
        // Full-width in CJK fonts despite living outside the CJK blocks:
        | 0x00B7             // · middle dot
        | 0x2014..=0x2015    // — ― dashes
        | 0x2018..=0x2019    // ‘ ’
        | 0x201C..=0x201D    // “ ”
        | 0x2026             // … ellipsis
    );
    if wide { 2 } else { 1 }
}

/// True where a line may break *before* `c` without splitting a word.
/// CJK breaks anywhere; Latin only at spaces (handled by the caller
/// tracking the last space).
fn is_cjk(c: char) -> bool {
    char_width(c) == 2
}

/// Characters that must not start a line (CJK closing punctuation) —
/// without this, a line ending exactly before "，" pushes the comma to
/// the next line, which reads as broken in Chinese typesetting.
fn forbidden_at_line_start(c: char) -> bool {
    matches!(c, '，' | '。' | '、' | '；' | '：' | '？' | '！' | '”' | '’' | '》' | '）' | '】' | '」' | '』' | '·' | '…' | ',' | '.' | ';' | ':' | '?' | '!' | ')' | ']' | '}')
}

/// Wraps one paragraph into lines of at most `width` columns. Returns
/// each line's text plus the character offset (relative to the
/// paragraph's start) where it begins.
fn wrap_paragraph(paragraph: &str, width: usize) -> Vec<(String, usize)> {
    let chars: Vec<char> = paragraph.chars().collect();
    if chars.is_empty() {
        return vec![(String::new(), 0)];
    }

    let mut lines = Vec::new();
    let mut line_start = 0usize;
    let mut cursor = 0usize;
    let mut columns = 0usize;
    // Where the current line could break at a space, if it has to.
    let mut last_space: Option<usize> = None;

    while cursor < chars.len() {
        let c = chars[cursor];
        let w = char_width(c);

        if columns + w > width && cursor > line_start {
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
            columns = 0;
            last_space = None;
            continue;
        }

        if c == ' ' {
            last_space = Some(cursor);
        }
        columns += w;
        cursor += 1;
    }

    if line_start < chars.len() {
        let text: String = chars[line_start..].iter().collect();
        lines.push((text.trim_end().to_string(), line_start));
    }
    lines
}

/// Lays `text` out into pages of `lines_per_page` lines, each at most
/// `width` columns wide.
pub fn paginate(text: &str, width: usize, lines_per_page: usize) -> Vec<Page> {
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
        // width 6 columns = 3 CJK chars per line
        let pages = paginate("一二三四五", 6, 10);
        assert_eq!(pages[0].lines, vec!["一二三", "四五"]);
    }

    #[test]
    fn wraps_latin_at_spaces() {
        let pages = paginate("hello world foo", 11, 10);
        assert_eq!(pages[0].lines, vec!["hello world", "foo"]);
    }

    #[test]
    fn does_not_start_a_line_with_closing_punctuation() {
        // Without the rule, "，" would be pushed to the next line.
        let pages = paginate("一二三，四", 6, 10);
        assert_eq!(pages[0].lines[0], "一二");
        assert!(pages[0].lines[1].starts_with("三，"));
    }

    #[test]
    fn splits_into_pages_of_requested_height() {
        let pages = paginate("一二三四五六七八九十", 2, 2);
        // 1 CJK char per line, 2 lines per page => 5 pages
        assert_eq!(pages.len(), 5);
        assert_eq!(pages[0].lines, vec!["一", "二"]);
        assert_eq!(pages[4].lines, vec!["九", "十"]);
    }

    #[test]
    fn page_ranges_are_contiguous_and_cover_everything() {
        let text = "第一段文字\n第二段文字\n第三段文字";
        let pages = paginate(text, 4, 2);
        assert_eq!(pages[0].start, 0);
        for w in pages.windows(2) {
            assert_eq!(w[0].end, w[1].start, "page ranges must be contiguous");
        }
        assert_eq!(pages.last().unwrap().end, text.chars().count());
    }

    #[test]
    fn paragraph_breaks_start_new_lines() {
        let pages = paginate("一\n二", 10, 10);
        assert_eq!(pages[0].lines, vec!["一", "二"]);
    }

    #[test]
    fn empty_text_yields_one_empty_page() {
        let pages = paginate("", 10, 10);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].lines, vec![""]);
    }

    #[test]
    fn zero_dimensions_yield_no_pages() {
        assert!(paginate("abc", 0, 10).is_empty());
        assert!(paginate("abc", 10, 0).is_empty());
    }
}
