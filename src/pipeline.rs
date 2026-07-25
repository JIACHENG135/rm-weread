//! The generation pipeline: chapters → decode → paginate → layout →
//! PDF → xochitl document (docs/design.md §"PDF 生成流水线与冻结规
//! 则"). This replaces the fullscreen QML reader as the project's
//! spine; the daemon is a thin trigger loop around these functions.
//!
//! Network failure policy: chapter *text* is required (no text, no
//! book), but underlines are decoration — a failed `/book/underlines`
//! call degrades to "no hot underlines in this chapter" instead of
//! failing the build, because the endpoint contract is the least
//! validated part of the stack (see underlines.rs).

use crate::layout::{self, BookLayout, ChapterInput, Grid};
use crate::xochitl_doc::{self, Delivery};
use crate::{login, paginate, pdfgen, reader, underlines, xhtml};
use std::fs;
use std::path::PathBuf;

/// Where everything lives. A struct so tests can point it at temp
/// dirs; the daemon uses `Paths::device()`.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Chapter cache + layout files + docs.json registry.
    pub data_dir: PathBuf,
    /// xochitl's document library.
    pub xochitl_dir: PathBuf,
    /// The QML-visible dir: layout.json for the popup's hit tests.
    pub exthome: PathBuf,
}

impl Paths {
    pub fn device() -> Self {
        Paths {
            data_dir: PathBuf::from("/home/root/.local/share/rm-weread"),
            xochitl_dir: PathBuf::from(xochitl_doc::XOCHITL_DIR),
            exthome: PathBuf::from("/home/root/xovi/exthome/weread"),
        }
    }
    pub fn registry(&self) -> PathBuf {
        self.data_dir.join("docs.json")
    }
    fn chapter_cache(&self, book_id: &str, chapter_uid: i64) -> PathBuf {
        self.data_dir.join("chapters").join(book_id).join(format!("{chapter_uid}.xhtml"))
    }
    pub fn layout_file(&self, book_id: &str) -> PathBuf {
        self.data_dir.join("layout").join(format!("{book_id}.json"))
    }
}

/// Fetches one chapter's XHTML, through the on-disk cache. The cache
/// is also what makes decoration refreshes cheap (and offline-safe):
/// the frozen geometry must be rebuilt from *exactly* the text that
/// froze it, never from a re-download that might differ.
fn chapter_xhtml(
    agent: &ureq::Agent,
    sess: &mut login::Session,
    paths: &Paths,
    book_id: &str,
    chapter: &reader::Chapter,
) -> Result<String, Box<dyn std::error::Error>> {
    let cache = paths.chapter_cache(book_id, chapter.chapter_uid);
    if let Ok(cached) = fs::read_to_string(&cache) {
        return Ok(cached);
    }
    let bytes = reader::fetch_chapter_content(agent, &mut sess.cookies, book_id, chapter)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if let Some(parent) = cache.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&cache, &text);
    Ok(text)
}

fn fetch_hot(
    agent: &ureq::Agent,
    api_key: &str,
    book_id: &str,
    chapter_uid: i64,
    text: &xhtml::Text,
) -> Vec<layout::HotInput> {
    match underlines::fetch_underlines(agent, api_key, book_id, chapter_uid) {
        Ok(u) => underlines::map_to_text(&u, text),
        Err(e) => {
            eprintln!("weread: underlines for chapter {chapter_uid} failed ({e}); continuing without");
            Vec::new()
        }
    }
}

/// Builds every chapter's input, fetching text (cached) and hot
/// underlines (best-effort). `progress` gets a human-readable note per
/// chapter — the daemon forwards it to the QML status popup.
fn build_chapters(
    agent: &ureq::Agent,
    sess: &mut login::Session,
    paths: &Paths,
    book_id: &str,
    chapters: &[reader::Chapter],
    grid: &Grid,
    mut progress: impl FnMut(&str),
) -> Result<Vec<ChapterInput>, Box<dyn std::error::Error>> {
    let mut inputs = Vec::new();
    for (i, ch) in chapters.iter().enumerate() {
        progress(&format!("章节 {}/{}: {}", i + 1, chapters.len(), ch.title));
        let xhtml_raw = chapter_xhtml(agent, sess, paths, book_id, ch)?;
        let text = xhtml::to_text(&xhtml_raw);
        let hot = fetch_hot(agent, &sess.api_key, book_id, ch.chapter_uid, &text);
        let pages = paginate::paginate(&text.text, grid.cols, grid.lines_per_page);
        inputs.push(ChapterInput {
            chapter_uid: ch.chapter_uid,
            title: ch.title.clone(),
            text: text.text,
            pages,
            hot,
        });
    }
    Ok(inputs)
}

pub struct Generated {
    pub layout: BookLayout,
    pub delivery: Delivery,
}

/// The whole pipeline for one book. Writes layout.json (data dir +
/// exthome copy for QML) and returns what happened.
pub fn generate_book(
    agent: &ureq::Agent,
    sess: &mut login::Session,
    paths: &Paths,
    book_id: &str,
    title: &str,
    author: &str,
    progress: impl FnMut(&str),
) -> Result<Generated, Box<dyn std::error::Error>> {
    reader::renew_session(agent, &mut sess.cookies)?;
    let chapters = reader::fetch_chapters(agent, &mut sess.cookies, book_id)?;
    if chapters.is_empty() {
        return Err("没有可读章节".into());
    }
    let grid = Grid::default();
    let inputs = build_chapters(agent, sess, paths, book_id, &chapters, &grid, progress)?;
    finish_build(paths, book_id, title, author, inputs, grid)
}

/// The offline tail of the pipeline — split out so a decoration
/// refresh (which re-reads cached text) shares it.
fn finish_build(
    paths: &Paths,
    book_id: &str,
    title: &str,
    author: &str,
    inputs: Vec<ChapterInput>,
    grid: Grid,
) -> Result<Generated, Box<dyn std::error::Error>> {
    let mut book_layout = layout::build(book_id, title, author, &inputs, grid);
    let pdf = pdfgen::generate(&book_layout, &inputs)?;
    let delivery = xochitl_doc::deliver(&paths.xochitl_dir, &paths.registry(), &book_layout, &pdf)?;
    book_layout.doc_uuid = match &delivery {
        Delivery::Created { uuid } | Delivery::Refreshed { uuid } | Delivery::Replaced { uuid } => uuid.clone(),
    };
    write_layout(paths, &book_layout)?;
    Ok(Generated { layout: book_layout, delivery })
}

fn write_layout(paths: &Paths, l: &BookLayout) -> std::io::Result<()> {
    let json = serde_json::to_string(l).unwrap_or_default();
    let per_book = paths.layout_file(&l.book_id);
    if let Some(parent) = per_book.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&per_book, &json)?;
    // The exthome copy is what the QML popup XHRs; single "current
    // book" slot for now (multi-book needs the current-document-uuid
    // lookup recorded as an on-device TODO in docs/design.md).
    fs::create_dir_all(&paths.exthome)?;
    let tmp = paths.exthome.join("layout.json.tmp");
    fs::write(&tmp, &json)?;
    fs::rename(tmp, paths.exthome.join("layout.json"))
}

pub fn load_layout(paths: &Paths, book_id: &str) -> Option<BookLayout> {
    let s = fs::read_to_string(paths.layout_file(book_id)).ok()?;
    serde_json::from_str(&s).ok()
}

/// Fraction of the hot-underline set that changed between the frozen
/// layout and a fresh fetch: |symmetric difference| / |union|, ranges
/// as identity. 0.0 = identical, 1.0 = disjoint.
pub fn hot_change_fraction(old: &BookLayout, fresh: &[(i64, Vec<String>)]) -> f32 {
    use std::collections::BTreeSet;
    let old_set: BTreeSet<(i64, &str)> = old
        .chapters
        .iter()
        .flat_map(|c| c.hot.iter().map(move |h| (c.chapter_uid, h.range.as_str())))
        .collect();
    let new_set: BTreeSet<(i64, &str)> = fresh
        .iter()
        .flat_map(|(uid, ranges)| ranges.iter().map(move |r| (*uid, r.as_str())))
        .collect();
    let union = old_set.union(&new_set).count();
    if union == 0 {
        return 0.0;
    }
    let sym_diff = old_set.symmetric_difference(&new_set).count();
    sym_diff as f32 / union as f32
}

/// How different the hot sets must be before a decoration refresh
/// actually rewrites the PDF (the design conversation settled on
/// "significant change only" to keep the risky swap-in-place rare).
pub const REFRESH_THRESHOLD: f32 = 0.2;

/// Daily decoration refresh for one already-generated book: re-fetch
/// underlines, and only if the set moved beyond the threshold, rebuild
/// the PDF from *cached* text (frozen geometry) and swap it in place.
/// Returns whether a rebuild happened.
pub fn refresh_decorations(
    agent: &ureq::Agent,
    sess: &mut login::Session,
    paths: &Paths,
    book_id: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let old = load_layout(paths, book_id).ok_or("no layout.json for this book — generate first")?;

    // Fresh hot sets, per chapter (range ids only, for the diff).
    let mut fresh_ranges = Vec::new();
    let mut fresh_hot: Vec<Vec<underlines::Underline>> = Vec::new();
    for c in &old.chapters {
        let u = underlines::fetch_underlines(agent, &sess.api_key, book_id, c.chapter_uid).unwrap_or_default();
        fresh_ranges.push((c.chapter_uid, u.iter().map(|x| x.range.clone()).collect::<Vec<_>>()));
        fresh_hot.push(u);
    }
    if hot_change_fraction(&old, &fresh_ranges) <= REFRESH_THRESHOLD {
        return Ok(false);
    }

    // Rebuild chapter inputs from the cache — never from the network,
    // so the geometry provably can't drift (content hash re-checked by
    // deliver() anyway).
    let mut inputs = Vec::new();
    for (c, hot) in old.chapters.iter().zip(fresh_hot) {
        let cache = paths.chapter_cache(book_id, c.chapter_uid);
        let raw = fs::read_to_string(&cache)
            .map_err(|e| format!("chapter cache missing for {}: {e} — cannot refresh frozen geometry", c.chapter_uid))?;
        let text = xhtml::to_text(&raw);
        let mapped = underlines::map_to_text(&hot, &text);
        inputs.push(ChapterInput {
            chapter_uid: c.chapter_uid,
            title: c.title.clone(),
            pages: paginate::paginate(&text.text, old.grid.cols, old.grid.lines_per_page),
            text: text.text,
            hot: mapped,
        });
    }
    let generated = finish_build(paths, book_id, &old.title, &old.author, inputs, old.grid)?;
    // A decoration refresh must never have changed geometry.
    if generated.layout.content_sha256 != old.content_sha256 {
        eprintln!(
            "weread: WARNING — refresh produced a different content hash for {book_id}; \
             cached text changed under us (delivered as a new/replaced document, ink protected)"
        );
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Grid, HotInput, build};
    use crate::paginate::paginate;

    fn layout_with_hot(ranges: &[(i64, &[&str])]) -> BookLayout {
        let grid = Grid { cols: 10, lines_per_page: 4, ..Grid::default() };
        let chapters: Vec<ChapterInput> = ranges
            .iter()
            .map(|(uid, rs)| {
                let text = "一二三四五六七八九十".repeat(3);
                ChapterInput {
                    chapter_uid: *uid,
                    title: format!("第{uid}章"),
                    pages: paginate(&text, grid.cols, grid.lines_per_page),
                    text,
                    hot: rs
                        .iter()
                        .enumerate()
                        .map(|(i, r)| HotInput { range: r.to_string(), off: i * 2, len: 2, count: 10 })
                        .collect(),
                }
            })
            .collect();
        build("b", "t", "a", &chapters, grid)
    }

    #[test]
    fn change_fraction_zero_for_identical_sets() {
        let l = layout_with_hot(&[(1, &["1-2", "3-4"])]);
        let fresh = vec![(1i64, vec!["1-2".to_string(), "3-4".to_string()])];
        assert_eq!(hot_change_fraction(&l, &fresh), 0.0);
    }

    #[test]
    fn change_fraction_one_for_disjoint_sets() {
        let l = layout_with_hot(&[(1, &["1-2"])]);
        let fresh = vec![(1i64, vec!["9-10".to_string()])];
        assert_eq!(hot_change_fraction(&l, &fresh), 1.0);
    }

    #[test]
    fn change_fraction_partial() {
        let l = layout_with_hot(&[(1, &["1-2", "3-4", "5-6"])]);
        // One of three replaced: union = 4, sym diff = 2.
        let fresh = vec![(1i64, vec!["1-2".to_string(), "3-4".to_string(), "7-8".to_string()])];
        assert_eq!(hot_change_fraction(&l, &fresh), 0.5);
    }

    #[test]
    fn change_fraction_handles_empty_both_sides() {
        let l = layout_with_hot(&[(1, &[])]);
        assert_eq!(hot_change_fraction(&l, &[]), 0.0);
    }
}
