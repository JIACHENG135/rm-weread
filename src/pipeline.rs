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
    /// Cover artwork, cached so a decoration refresh rebuilds the exact
    /// same page 0 without the network.
    pub fn cover_file(&self, book_id: &str) -> PathBuf {
        self.data_dir.join("covers").join(format!("{book_id}.jpg"))
    }
}

/// Downloads an image once and caches it at `cached`. Failure is not
/// fatal: a book without artwork still gets a typeset title page, and
/// `has_cover` stays true either way so the page numbering can't wobble
/// with network conditions.
///
/// The destination is a parameter rather than derived from the book id
/// because the same book has two covers at two sizes — the shelf
/// thumbnail the browser lists, and the largest variant the PDF embeds.
/// Sharing one cache slot would silently pin whichever was fetched
/// first, and in practice that is the small one.
pub fn fetch_cover(agent: &ureq::Agent, cached: &std::path::Path, url: &str) -> Option<Vec<u8>> {
    if let Ok(bytes) = fs::read(cached) {
        return Some(bytes);
    }
    if url.is_empty() {
        return None;
    }
    let bytes = (|| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut resp = agent.get(url).call()?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut resp.body_mut().as_reader(), &mut buf)?;
        Ok(buf)
    })();
    match bytes {
        Ok(b) if !b.is_empty() => {
            if let Some(parent) = cached.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(cached, &b);
            Some(b)
        }
        Ok(_) => None,
        Err(e) => {
            eprintln!("weread: cover download failed ({url}): {e}; using a text-only cover");
            None
        }
    }
}

/// WeRead serves covers in numbered size variants (`t6_`, `t7_`…), and
/// the shelf hands back a small one. `t9_` is the largest that exists —
/// still only ~428×616, which is why the cover page composes the art
/// rather than bleeding it to the edges.
pub fn largest_cover_url(url: &str) -> String {
    match url.rfind('/') {
        Some(slash) => {
            let (dir, file) = url.split_at(slash + 1);
            let bumped = match file.split_once('_') {
                Some((prefix, rest))
                    if prefix.starts_with('t') && prefix[1..].chars().all(|c| c.is_ascii_digit()) =>
                {
                    format!("t9_{rest}")
                }
                _ => file.to_string(),
            };
            format!("{dir}{bumped}")
        }
        None => url.to_string(),
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

/// Underlines for one chapter, falling back to whatever the previous
/// build recorded when the call fails.
///
/// Without the fallback, throttling quietly *deletes* decoration: a real
/// regeneration hit 45 HTTP 499s and came back with 655 of the book's
/// 840 underlines, which then swapped in place over the good version.
/// Underlines are still optional — a chapter that has never been fetched
/// successfully just has none — but a failed call must never be allowed
/// to look like "this chapter has fewer underlines now".
fn fetch_hot(
    agent: &ureq::Agent,
    api_key: &str,
    book_id: &str,
    chapter_uid: i64,
    text: &xhtml::Text,
    previous: Option<&[layout::Hot]>,
) -> Vec<layout::HotInput> {
    match underlines::fetch_underlines(agent, api_key, book_id, chapter_uid) {
        Ok(u) => underlines::map_to_text(&u, text),
        Err(e) => {
            let kept: Vec<layout::HotInput> = previous
                .unwrap_or_default()
                .iter()
                .map(|h| layout::HotInput {
                    range: h.range.clone(),
                    off: h.off,
                    len: h.len,
                    count: h.count,
                })
                .collect();
            if kept.is_empty() {
                eprintln!("weread: underlines for chapter {chapter_uid} failed ({e}); continuing without");
            } else {
                eprintln!(
                    "weread: underlines for chapter {chapter_uid} failed ({e}); \
                     keeping the {} already known rather than dropping them",
                    kept.len()
                );
            }
            kept
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
    // What the last successful build knew, so a throttled call this time
    // degrades to "unchanged" instead of "gone".
    let known: std::collections::HashMap<i64, Vec<layout::Hot>> = load_layout(paths, book_id)
        .map(|l| l.chapters.into_iter().map(|c| (c.chapter_uid, c.hot)).collect())
        .unwrap_or_default();

    let mut inputs = Vec::new();
    for (i, ch) in chapters.iter().enumerate() {
        progress(&format!("章节 {}/{}: {}", i + 1, chapters.len(), ch.title));
        let xhtml_raw = chapter_xhtml(agent, sess, paths, book_id, ch)?;
        let text = xhtml::to_text(&xhtml_raw);
        let hot = fetch_hot(
            agent,
            &sess.api_key,
            book_id,
            ch.chapter_uid,
            &text,
            known.get(&ch.chapter_uid).map(Vec::as_slice),
        );
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
    cover_url: &str,
    progress: impl FnMut(&str),
) -> Result<Generated, Box<dyn std::error::Error>> {
    reader::renew_session(agent, &mut sess.cookies)?;
    let chapters = reader::fetch_chapters(agent, &mut sess.cookies, book_id)?;
    if chapters.is_empty() {
        return Err("没有可读章节".into());
    }
    let grid = Grid::default();
    let cover = fetch_cover(agent, &paths.cover_file(book_id), &largest_cover_url(cover_url));
    let inputs = build_chapters(agent, sess, paths, book_id, &chapters, &grid, progress)?;
    // A title page is always page 0, artwork or not — see fetch_cover.
    finish_build(paths, book_id, title, author, inputs, grid, true, cover)
}

/// The offline tail of the pipeline — split out so a decoration
/// refresh (which re-reads cached text) shares it.
#[allow(clippy::too_many_arguments)]
fn finish_build(
    paths: &Paths,
    book_id: &str,
    title: &str,
    author: &str,
    inputs: Vec<ChapterInput>,
    grid: Grid,
    has_cover: bool,
    cover: Option<Vec<u8>>,
) -> Result<Generated, Box<dyn std::error::Error>> {
    let mut book_layout = layout::build(book_id, title, author, &inputs, grid, has_cover);
    let pdf = pdfgen::generate(&book_layout, &inputs, cover.as_deref())?;
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

    // The QML copy is keyed by *document* uuid, not book id: the popup
    // knows which document is open (`view.document.id`) and nothing
    // else, so this is the one name it can look up directly. Keying it
    // this way is also what lets several generated books coexist —
    // there is no "current book" slot any more.
    let dir = paths.exthome.join("layout");
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{}.json.tmp", l.doc_uuid));
    fs::write(&tmp, &json)?;
    fs::rename(tmp, dir.join(format!("{}.json", l.doc_uuid)))
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
    // Cache only: re-downloading here could hand us different bytes, and
    // a *missing* cover would silently drop page 0 and shift the whole
    // book under the reader's ink.
    let cover = fs::read(paths.cover_file(book_id)).ok();
    let generated =
        finish_build(paths, book_id, &old.title, &old.author, inputs, old.grid, old.cover, cover)?;
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

    #[test]
    fn cover_urls_are_bumped_to_the_largest_variant() {
        // t9_ is the biggest WeRead serves; t10_ and up 404.
        assert_eq!(
            largest_cover_url("https://cdn.weread.qq.com/weread/cover/88/X_1/t6_X_1.jpg"),
            "https://cdn.weread.qq.com/weread/cover/88/X_1/t9_X_1.jpg"
        );
        // Already largest: unchanged.
        assert_eq!(
            largest_cover_url("https://c/x/t9_a.jpg"),
            "https://c/x/t9_a.jpg"
        );
        // Not a size-prefixed name (audiobook covers, the articles
        // pseudo-entry): left exactly alone rather than guessed at.
        assert_eq!(largest_cover_url("https://c/x/s_abc.png"), "https://c/x/s_abc.png");
        assert_eq!(largest_cover_url("https://c/x/plain.jpg"), "https://c/x/plain.jpg");
        assert_eq!(largest_cover_url(""), "");
    }
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
        build("b", "t", "a", &chapters, grid, false)
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
