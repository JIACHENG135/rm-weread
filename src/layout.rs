//! Frozen page geometry for the generated PDF, persisted as
//! `layout.json` — the single authority on where every character sits
//! (docs/design.md §"PDF 生成流水线与冻结规则").
//!
//! The model is deliberately smaller than "store every character's
//! coordinates": layout is a uniform character grid (paginate.rs's
//! column model), so a line's box is fully derived from its row index
//! and the grid constants. What must be *stored* is only what can't be
//! recomputed cheaply at query time: each line's character offset, and
//! a box around every underlined run (which the QML popup hit-tests
//! against — touching the underlined words is what opens the reviews,
//! so the underline is its own affordance and there is no marker glyph).
//!
//! Coordinates in this file are normalized to 0..1 with a **top-left
//! origin** — the convention touch events arrive in on the QML side.
//! rM2 (1404×1872) and Paper Pro (1620×2160) are both 3:4, and the PDF
//! page is generated 3:4 too, so normalized coordinates are
//! device-independent (the conversation that produced this design
//! called this out explicitly: don't store pixels or points).
//!
//! Freezing: `content_sha256` is the hash of the decoded chapter texts
//! plus the grid constants. Ink is anchored to page geometry, so a
//! regenerated PDF may only be swapped under an existing document when
//! this hash is unchanged (decoration-only refresh); a changed hash
//! must produce a *new* document (xochitl_doc.rs enforces this).

use crate::paginate::{self, Page};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// PDF page size in points. 702×936 is 3:4 at a comfortable print-ish
/// scale (≈9.75in × 13in at 72dpi — xochitl scales it to the screen).
pub const PAGE_W_PT: f32 = 702.0;
pub const PAGE_H_PT: f32 = 936.0;

/// Where a line's baseline sits inside its line box, as a fraction of
/// the line height from the box top. Tuned for Noto Sans CJK's vertical
/// metrics (ascent 1160/1000 em units, descent -320).
pub const BASELINE_FACTOR: f32 = 0.78;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    pub font_pt: f32,
    pub margin_x_pt: f32,
    pub margin_top_pt: f32,
    pub margin_bottom_pt: f32,
    /// Text width in paginate.rs columns (a CJK glyph is 2 columns, one
    /// column is half an em).
    pub cols: usize,
    pub lines_per_page: usize,
}

impl Default for Grid {
    fn default() -> Self {
        // 58 cols × 10pt/col = 580pt of text; 27 lines × 30pt = 810pt.
        // Both fit inside 702×936 with the margins below.
        Grid {
            font_pt: 20.0,
            margin_x_pt: 54.0,
            margin_top_pt: 72.0,
            margin_bottom_pt: 54.0,
            cols: 58,
            lines_per_page: 27,
        }
    }
}

impl Grid {
    pub fn col_pt(&self) -> f32 {
        self.font_pt / 2.0
    }
    pub fn line_pt(&self) -> f32 {
        self.font_pt * 1.5
    }
    /// Top edge of row `row`'s line box, in points from the page top.
    pub fn line_top_pt(&self, row: usize) -> f32 {
        self.margin_top_pt + row as f32 * self.line_pt()
    }
    /// Baseline of row `row`, in points from the page top.
    pub fn baseline_pt(&self, row: usize) -> f32 {
        self.line_top_pt(row) + BASELINE_FACTOR * self.line_pt()
    }
    /// Left edge of column `col`, in points from the page left.
    pub fn col_x_pt(&self, col: usize) -> f32 {
        self.margin_x_pt + col as f32 * self.col_pt()
    }
}

/// Column index (relative to the line start) where the `char_idx`-th
/// character of `line_text` begins.
pub fn col_of_char(line_text: &str, char_idx: usize) -> usize {
    line_text
        .chars()
        .take(char_idx)
        .map(paginate::char_width)
        .sum()
}

/// One laid-out body line. Geometry is derived (`Grid`), so only the
/// offsets are stored. `page` is absolute in the PDF, `row` is the line
/// slot on that page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub page: usize,
    pub row: usize,
    /// Chapter plain-text character offset of the line's first char.
    pub off: usize,
    /// Character count of the line as rendered.
    pub len: usize,
}

/// One hot underline, in chapter plain-text offsets. `range` is
/// WeRead's own id ("345-361", raw-HTML rune offsets) — kept verbatim
/// because it's the key `/book/readreviews` wants back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hot {
    pub range: String,
    pub off: usize,
    pub len: usize,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChapterLayout {
    pub chapter_uid: i64,
    pub title: String,
    /// Absolute 0-based PDF page the chapter starts on.
    pub page_start: usize,
    pub page_count: usize,
    pub lines: Vec<Line>,
    pub hot: Vec<Hot>,
}

/// Flattened, QML-friendly view of every tappable region in the book:
/// one entry per underlined run on one line, boxed around the *glyphs*
/// so the reader taps the underlined words themselves. The popup reads
/// layout.json directly (XHR) and hit-tests taps against these without
/// needing the per-chapter structure.
///
/// A range that wraps across lines or pages contributes several taps
/// sharing the same `range` — each one opens the same reviews.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tap {
    pub page: usize,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub chapter_uid: i64,
    pub range: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookLayout {
    pub v: u32,
    /// Whether page 0 is a cover page rather than the first chapter.
    #[serde(default)]
    pub cover: bool,
    pub book_id: String,
    pub title: String,
    pub author: String,
    /// Filled in at delivery time (xochitl_doc.rs) — empty until then.
    #[serde(default)]
    pub doc_uuid: String,
    pub content_sha256: String,
    pub page_w_pt: f32,
    pub page_h_pt: f32,
    pub grid: Grid,
    pub page_count: usize,
    pub chapters: Vec<ChapterLayout>,
    pub taps: Vec<Tap>,
}

/// Per-chapter input to `build`: the paginated text plus the hot
/// underlines already mapped to plain-text offsets (underlines.rs).
pub struct ChapterInput {
    pub chapter_uid: i64,
    pub title: String,
    /// The chapter's plain text (xhtml::to_text output) — hashed into
    /// `content_sha256`, not stored.
    pub text: String,
    pub pages: Vec<Page>,
    pub hot: Vec<HotInput>,
}

pub struct HotInput {
    pub range: String,
    pub off: usize,
    pub len: usize,
    pub count: u32,
}

/// A contiguous stretch of one underline on one line of one page —
/// what pdfgen actually draws. Chapter-local page index.
#[derive(Debug, Clone, PartialEq)]
pub struct UnderlineSeg {
    pub page: usize,
    pub row: usize,
    pub col_start: usize,
    /// Exclusive.
    pub col_end: usize,
}

/// Computes where a hot range [off, off+len) lands on the laid-out
/// pages, as per-line column segments.
pub fn underline_segments(pages: &[Page], off: usize, len: usize) -> Vec<UnderlineSeg> {
    let end = off + len;
    let mut segs = Vec::new();
    for (pi, page) in pages.iter().enumerate() {
        // Cheap reject: the page-level range is contiguous and covers
        // its lines.
        if end <= page.start || off >= page.end {
            continue;
        }
        for (row, (line, &line_start)) in page.lines.iter().zip(&page.line_starts).enumerate() {
            let line_len = line.chars().count();
            let line_end = line_start + line_len;
            let s = off.max(line_start);
            let e = end.min(line_end);
            if s >= e {
                continue;
            }
            segs.push(UnderlineSeg {
                page: pi,
                row,
                col_start: col_of_char(line, s - line_start),
                col_end: col_of_char(line, e - line_start),
            });
        }
    }
    segs
}

/// Content hash that freezes the geometry: decoded chapter texts plus
/// every grid constant that influences layout. Any change here must
/// open a new xochitl document.
/// Bumped whenever pagination itself changes shape — a new character
/// width rule, a different line-breaking decision, anything that moves
/// text on the page.
///
/// It has to be in the hash. The grid constants alone don't describe
/// the *algorithm*, so without this a pagination fix would keep the old
/// hash, xochitl_doc would treat the rebuild as a decoration-only
/// refresh, and it would swap re-flowed pages under ink anchored to the
/// old ones. v2: curly quotes, ellipsis and dashes became full-width.
pub const LAYOUT_ALGO_VERSION: u32 = 2;

pub fn content_hash(chapters: &[ChapterInput], grid: &Grid, has_cover: bool) -> String {
    let mut h = Sha256::new();
    h.update(format!("algo:{LAYOUT_ALGO_VERSION}\n"));
    // `has_cover` is in here because a cover occupies page 0 and pushes
    // every chapter page along by one — that is a geometry change, and
    // ink anchored to the old numbering would land on the wrong page.
    // The cover *image bytes* deliberately are not: swapping artwork
    // inside a fixed box moves nothing, so a new cover is allowed to
    // ride in on a decoration refresh.
    h.update(format!(
        "cover:{has_cover}\ngrid:{}:{}:{}:{}:{}:{}:{}x{}\n",
        grid.font_pt,
        grid.margin_x_pt,
        grid.margin_top_pt,
        grid.margin_bottom_pt,
        grid.cols,
        grid.lines_per_page,
        PAGE_W_PT,
        PAGE_H_PT,
    ));
    for c in chapters {
        h.update(c.chapter_uid.to_le_bytes());
        h.update([0u8]);
        h.update(c.text.as_bytes());
        h.update([0u8]);
    }
    let digest = h.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// How far below its line box a tap still counts as hitting the run,
/// as a fraction of the line height. The rule is stroked just under the
/// baseline, near the bottom of the box, and fingers land low — without
/// a little slack the most natural gesture (touch the line itself)
/// falls into the gap between rows.
const TAP_SLOP_BELOW: f32 = 0.3;

/// Builds the frozen layout for a whole book. Chapter order is PDF
/// order; each chapter starts on a fresh page.
pub fn build(
    book_id: &str,
    title: &str,
    author: &str,
    chapters: &[ChapterInput],
    grid: Grid,
    has_cover: bool,
) -> BookLayout {
    let content_sha256 = content_hash(chapters, &grid, has_cover);
    let mut out_chapters = Vec::new();
    let mut taps = Vec::new();
    // The cover, when there is one, is page 0; chapters start after it.
    // Everything downstream (lines, taps, outline destinations) works in
    // absolute pages, so this single offset is the whole change.
    let mut page_cursor = usize::from(has_cover);

    for c in chapters {
        let mut lines = Vec::new();
        for (pi, page) in c.pages.iter().enumerate() {
            for (row, (line, &start)) in page.lines.iter().zip(&page.line_starts).enumerate() {
                lines.push(Line {
                    page: page_cursor + pi,
                    row,
                    off: start,
                    len: line.chars().count(),
                });
            }
        }

        let mut hot_out = Vec::new();
        // Deterministic order: by text position, not by whatever order
        // the API returned — `taps` is part of the frozen artifact.
        let mut hot_sorted: Vec<&HotInput> = c.hot.iter().collect();
        hot_sorted.sort_by_key(|h| (h.off, h.len));
        for h in hot_sorted {
            // Every underlined run is its own tap target; a range that
            // wraps just yields several boxes with the same `range`.
            for seg in underline_segments(&c.pages, h.off, h.len) {
                taps.push(tap_box(
                    &grid,
                    page_cursor + seg.page,
                    seg.row,
                    seg.col_start,
                    seg.col_end,
                    c.chapter_uid,
                    &h.range,
                    h.count,
                ));
            }
            hot_out.push(Hot {
                range: h.range.clone(),
                off: h.off,
                len: h.len,
                count: h.count,
            });
        }

        out_chapters.push(ChapterLayout {
            chapter_uid: c.chapter_uid,
            title: c.title.clone(),
            page_start: page_cursor,
            page_count: c.pages.len(),
            lines,
            hot: hot_out,
        });
        page_cursor += c.pages.len();
    }

    BookLayout {
        // v2: tappable underline runs (`taps`) replaced the circled-digit
        // markers of v1. The QML popup reads this field name directly.
        v: 2,
        cover: has_cover,
        book_id: book_id.to_string(),
        title: title.to_string(),
        author: author.to_string(),
        doc_uuid: String::new(),
        content_sha256,
        page_w_pt: PAGE_W_PT,
        page_h_pt: PAGE_H_PT,
        grid,
        page_count: page_cursor,
        chapters: out_chapters,
        taps,
    }
}

/// Box around one underlined run: exactly the columns the run covers,
/// and its line box vertically, extended a little downward (see
/// `TAP_SLOP_BELOW`). Normalized, top-left origin.
#[allow(clippy::too_many_arguments)]
fn tap_box(
    grid: &Grid,
    page: usize,
    row: usize,
    col_start: usize,
    col_end: usize,
    chapter_uid: i64,
    range: &str,
    count: u32,
) -> Tap {
    let top = grid.line_top_pt(row);
    Tap {
        page,
        x0: grid.col_x_pt(col_start) / PAGE_W_PT,
        x1: grid.col_x_pt(col_end.min(grid.cols)) / PAGE_W_PT,
        y0: top / PAGE_H_PT,
        y1: (top + grid.line_pt() * (1.0 + TAP_SLOP_BELOW)) / PAGE_H_PT,
        chapter_uid,
        range: range.to_string(),
        count,
    }
}

impl BookLayout {
    /// How many hot underlines the book has. Not `taps.len()`: a range
    /// that wraps across lines contributes one tap box per line, so
    /// that number is always the larger one.
    pub fn hot_count(&self) -> usize {
        self.chapters.iter().map(|c| c.hot.len()).sum()
    }

    /// Hit-tests a tap (normalized, top-left origin) against the
    /// underlined runs on `page`. Boxes overlap by design — a wrapped
    /// range stacks on consecutive rows, and `TAP_SLOP_BELOW` lets each
    /// row reach into the next — so ties go to the nearest centre.
    pub fn hit_tap(&self, page: usize, x: f32, y: f32) -> Option<&Tap> {
        self.taps
            .iter()
            .filter(|t| t.page == page)
            .filter(|t| x >= t.x0 && x <= t.x1 && y >= t.y0 && y <= t.y1)
            .min_by(|a, b| {
                let da = (x - (a.x0 + a.x1) / 2.0).abs() + (y - (a.y0 + a.y1) / 2.0).abs();
                let db = (x - (b.x0 + b.x1) / 2.0).abs() + (y - (b.y0 + b.y1) / 2.0).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paginate::paginate;

    fn chapter(uid: i64, text: &str, hot: Vec<HotInput>) -> ChapterInput {
        let grid = Grid { cols: 6, lines_per_page: 2, ..Grid::default() };
        ChapterInput {
            chapter_uid: uid,
            title: format!("第{uid}章"),
            text: text.to_string(),
            pages: paginate(text, grid.cols, grid.lines_per_page),
            hot,
        }
    }

    fn hot(range: &str, off: usize, len: usize, count: u32) -> HotInput {
        HotInput { range: range.to_string(), off, len, count }
    }

    #[test]
    fn underline_segments_split_across_lines_and_pages() {
        // 3 CJK chars per line, 2 lines per page.
        let pages = paginate("一二三四五六七八九十", 6, 2);
        // chars 2..8 span line 2 of page 0 through line 1 of page 1.
        let segs = underline_segments(&pages, 2, 6);
        assert_eq!(
            segs,
            vec![
                UnderlineSeg { page: 0, row: 0, col_start: 4, col_end: 6 },
                UnderlineSeg { page: 0, row: 1, col_start: 0, col_end: 6 },
                UnderlineSeg { page: 1, row: 0, col_start: 0, col_end: 4 },
            ]
        );
    }

    #[test]
    fn underline_columns_account_for_narrow_latin() {
        let pages = paginate("ab一二", 6, 2);
        // Underline the "一" — it starts after two 1-column chars.
        let segs = underline_segments(&pages, 2, 1);
        assert_eq!(segs, vec![UnderlineSeg { page: 0, row: 0, col_start: 2, col_end: 4 }]);
    }

    #[test]
    fn build_assigns_absolute_pages_across_chapters() {
        let grid = Grid { cols: 6, lines_per_page: 2, ..Grid::default() };
        let c1 = ChapterInput { pages: paginate("一二三四五六七", 6, 2), ..chapter(1, "一二三四五六七", vec![]) };
        let c2 = ChapterInput { pages: paginate("八九", 6, 2), ..chapter(2, "八九", vec![]) };
        let l = build("b", "t", "a", &[c1, c2], grid, false);
        assert_eq!(l.chapters[0].page_start, 0);
        assert_eq!(l.chapters[0].page_count, 2);
        assert_eq!(l.chapters[1].page_start, 2);
        assert_eq!(l.page_count, 3);
        // Second chapter's lines carry absolute page numbers.
        assert!(l.chapters[1].lines.iter().all(|line| line.page == 2));
    }

    #[test]
    fn taps_follow_text_order_and_are_hit_testable() {
        let grid = Grid { cols: 6, lines_per_page: 2, ..Grid::default() };
        let text = "一二三四五六七八九十";
        let c = ChapterInput {
            pages: paginate(text, grid.cols, grid.lines_per_page),
            // CJK is 2 columns wide, so cols:6 holds 3 chars per line
            // and a page holds 6 — both underlines have to start inside
            // that first page for this to be about ordering rather than
            // about the per-page numbering reset.
            ..chapter(1, text, vec![hot("30-40", 2, 2, 500), hot("10-20", 0, 2, 100)])
        };
        let l = build("b", "t", "a", &[c], grid, false);
        // Sorted by offset: "10-20" gets ①, "30-40" gets ②.
        assert_eq!(l.chapters[0].hot[0].range, "10-20");
        // Tapping the underlined glyphs themselves opens that range.
        let t = &l.taps[0];
        assert_eq!(t.range, "10-20");
        let hit = l.hit_tap(t.page, (t.x0 + t.x1) / 2.0, (t.y0 + t.y1) / 2.0).unwrap();
        assert_eq!(hit.range, "10-20");
        // A tap on a different page misses.
        assert!(l.hit_tap(t.page + 7, (t.x0 + t.x1) / 2.0, (t.y0 + t.y1) / 2.0).is_none());
        // So does one in the margin, left of the text column.
        assert!(l.hit_tap(t.page, 0.01, (t.y0 + t.y1) / 2.0).is_none());
    }

    #[test]
    fn a_wrapped_range_yields_one_tap_per_line_sharing_the_range() {
        // cols:6 with 2-column CJK holds 3 chars per line, so a 4-char
        // underline starting mid-line necessarily wraps.
        let grid = Grid { cols: 6, lines_per_page: 2, ..Grid::default() };
        let text = "一二三四五六";
        let c = ChapterInput {
            pages: paginate(text, grid.cols, grid.lines_per_page),
            ..chapter(1, text, vec![hot("7-8", 2, 3, 42)])
        };
        let l = build("b", "t", "a", &[c], grid, false);
        assert!(l.taps.len() > 1, "expected the range to wrap");
        assert!(l.taps.iter().all(|t| t.range == "7-8" && t.count == 42));
        // Each line's box is tappable and resolves to the same range.
        for t in &l.taps {
            let hit = l.hit_tap(t.page, (t.x0 + t.x1) / 2.0, (t.y0 + t.y1) / 2.0).unwrap();
            assert_eq!(hit.range, "7-8");
        }
    }

    #[test]
    fn every_underline_is_tappable_however_dense_the_chapter() {
        // The regression this replaced marker glyphs to fix: circled
        // digits capped a chapter at 20 tap targets, so a real book
        // (《球状闪电》: 840 hot underlines, 151 in one chapter) left
        // 62% of its underlines dead. Nothing is capped now.
        let grid = Grid { cols: 60, lines_per_page: 20, ..Grid::default() };
        let text: String = "一二三四五六七八九十".repeat(100);
        let hots: Vec<HotInput> = (0..151).map(|i| hot(&format!("r{i}"), i * 4, 2, 10)).collect();
        let c = ChapterInput { pages: paginate(&text, grid.cols, grid.lines_per_page), ..chapter(1, &text, hots) };
        let l = build("b", "t", "a", &[c], grid, false);

        assert_eq!(l.chapters[0].hot.len(), 151);
        let ranges: std::collections::BTreeSet<&str> = l.taps.iter().map(|t| t.range.as_str()).collect();
        assert_eq!(ranges.len(), 151, "every range must have at least one tap target");
        // And each one is actually reachable at its own centre.
        for t in &l.taps {
            assert!(l.hit_tap(t.page, (t.x0 + t.x1) / 2.0, (t.y0 + t.y1) / 2.0).is_some());
        }
    }

    #[test]
    fn content_hash_covers_the_pagination_algorithm() {
        // A pagination change re-flows text. If it didn't move the hash,
        // xochitl_doc would swap the new pages in under existing ink.
        let grid = Grid::default();
        let a = [chapter(1, "一二三", vec![])];
        let h = content_hash(&a, &grid, false);
        assert!(h.len() == 64);
        // The version is genuinely mixed in, not decoration.
        let mut alt = Sha256::new();
        alt.update(format!("algo:{}\n", LAYOUT_ALGO_VERSION + 1));
        assert_ne!(format!("{:x}", alt.finalize())[..8].to_string(), h[..8].to_string());
    }

    #[test]
    fn content_hash_freezes_text_and_grid() {
        let grid = Grid::default();
        let a = [chapter(1, "一二三", vec![])];
        let b = [chapter(1, "一二四", vec![])];
        assert_ne!(content_hash(&a, &grid, false), content_hash(&b, &grid, false));
        let mut grid2 = grid;
        grid2.font_pt = 22.0;
        assert_ne!(content_hash(&a, &grid, false), content_hash(&a, &grid2, false));
        assert_eq!(content_hash(&a, &grid, false), content_hash(&a, &grid, false));
    }

    #[test]
    fn layout_json_roundtrips() {
        let grid = Grid { cols: 6, lines_per_page: 2, ..Grid::default() };
        let text = "一二三四五";
        let c = ChapterInput { pages: paginate(text, 6, 2), ..chapter(1, text, vec![hot("1-2", 0, 2, 3)]) };
        let l = build("b", "题", "作者", &[c], grid, false);
        let json = serde_json::to_string(&l).unwrap();
        let back: BookLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(l, back);
    }
}
