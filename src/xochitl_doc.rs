//! Delivers a generated PDF into xochitl's document library and
//! enforces the freeze rules (docs/design.md §"PDF 生成流水线与冻结规
//! 则"). Ink (.rm files) anchors to page geometry, so the one thing
//! this module must never do is silently change geometry under an
//! existing document:
//!
//! - same `content_sha256` → decoration-only refresh: overwrite the
//!   existing `<uuid>.pdf` in place (and drop stale thumbnails)
//! - changed hash, document has no ink yet → full overwrite is fine
//! - changed hash, document HAS ink → refuse to touch it; create a new
//!   document with a versioned name and leave the old one to the user
//!
//! A tiny registry (`docs.json` under the rm-weread data dir) remembers
//! book → (uuid, hash) across runs.
//!
//! Known unknown, recorded here as in the design discussion: whether
//! xochitl picks up an in-place `.pdf` swap without a restart (it may
//! cache renders/thumbnails), and whether new files appear without
//! `systemctl restart xochitl`. Deleting `<uuid>.thumbnails` and
//! bumping `.metadata`'s lastModified is the best available nudge; the
//! rest needs on-device verification.

use crate::layout::BookLayout;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const XOCHITL_DIR: &str = "/home/root/.local/share/remarkable/xochitl";
pub const REGISTRY_PATH: &str = "/home/root/.local/share/rm-weread/docs.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    /// book_id → delivered document.
    pub books: BTreeMap<String, DeliveredDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveredDoc {
    pub uuid: String,
    pub content_sha256: String,
    pub visible_name: String,
    /// Unix seconds of the last underline-decoration refresh.
    #[serde(default)]
    pub decorations_refreshed_at: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Delivery {
    /// New document created (first generation, or geometry changed on
    /// an inked document → versioned sibling).
    Created { uuid: String },
    /// Existing document's PDF swapped in place (same geometry).
    Refreshed { uuid: String },
    /// Existing document overwritten wholesale (geometry changed, no
    /// ink present).
    Replaced { uuid: String },
}

pub fn load_registry(path: &Path) -> Registry {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_registry(path: &Path, reg: &Registry) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(reg).unwrap_or_default())?;
    fs::rename(&tmp, path)
}

/// Version-4 UUID from /dev/urandom — no dependency needed for one id.
fn new_uuid() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// True when the document's directory contains any pen strokes.
fn has_ink(xochitl_dir: &Path, uuid: &str) -> bool {
    let dir = xochitl_dir.join(uuid);
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().map(|x| x == "rm").unwrap_or(false))
        })
        .unwrap_or(false)
}

fn write_metadata(xochitl_dir: &Path, uuid: &str, visible_name: &str) -> std::io::Result<()> {
    let metadata = serde_json::json!({
        "visibleName": visible_name,
        "type": "DocumentType",
        "parent": "",
        "lastModified": now_ms().to_string(),
        "lastOpened": "",
        "lastOpenedPage": 0,
        "version": 1,
        "pinned": false,
        "synced": false,
        "modified": false,
        "deleted": false,
        "metadatamodified": false
    });
    fs::write(
        xochitl_dir.join(format!("{uuid}.metadata")),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
}

fn write_content_file(xochitl_dir: &Path, uuid: &str, page_count: usize) -> std::io::Result<()> {
    let content = serde_json::json!({
        "fileType": "pdf",
        "coverPageNumber": 0,
        "documentMetadata": {},
        "extraMetadata": {},
        "fontName": "",
        "lineHeight": -1,
        "margins": 100,
        "orientation": "portrait",
        "pageCount": page_count,
        "textScale": 1,
        "transform": {}
    });
    fs::write(
        xochitl_dir.join(format!("{uuid}.content")),
        serde_json::to_string_pretty(&content).unwrap(),
    )
}

fn write_pdf(xochitl_dir: &Path, uuid: &str, pdf: &[u8]) -> std::io::Result<()> {
    // Atomic within the same filesystem, so xochitl never sees a
    // half-written file.
    let tmp = xochitl_dir.join(format!("{uuid}.pdf.tmp"));
    fs::write(&tmp, pdf)?;
    fs::rename(&tmp, xochitl_dir.join(format!("{uuid}.pdf")))
}

fn drop_thumbnails(xochitl_dir: &Path, uuid: &str) {
    let _ = fs::remove_dir_all(xochitl_dir.join(format!("{uuid}.thumbnails")));
}

/// Delivers `pdf` for `layout`'s book, applying the freeze rules. Pass
/// the real `XOCHITL_DIR`/`REGISTRY_PATH` on device; tests use temp
/// dirs. On success the registry has been updated on disk and
/// `layout.doc_uuid` is reflected in the returned value (the caller
/// should set it before persisting layout.json).
pub fn deliver(
    xochitl_dir: &Path,
    registry_path: &Path,
    layout: &BookLayout,
    pdf: &[u8],
) -> Result<Delivery, Box<dyn std::error::Error>> {
    fs::create_dir_all(xochitl_dir)?;
    let mut reg = load_registry(registry_path);
    let visible_name = format!("{} — 微信读书", layout.title);

    let delivery = match reg.books.get(&layout.book_id) {
        Some(doc) if doc.content_sha256 == layout.content_sha256 => {
            // Geometry unchanged: decoration-only refresh, ink is safe.
            write_pdf(xochitl_dir, &doc.uuid, pdf)?;
            drop_thumbnails(xochitl_dir, &doc.uuid);
            // Nudge xochitl's change detection.
            write_metadata(xochitl_dir, &doc.uuid, &doc.visible_name)?;
            Delivery::Refreshed { uuid: doc.uuid.clone() }
        }
        Some(doc) if !has_ink(xochitl_dir, &doc.uuid) => {
            // Geometry changed but nothing is anchored to it yet.
            write_pdf(xochitl_dir, &doc.uuid, pdf)?;
            write_content_file(xochitl_dir, &doc.uuid, layout.page_count)?;
            write_metadata(xochitl_dir, &doc.uuid, &visible_name)?;
            drop_thumbnails(xochitl_dir, &doc.uuid);
            Delivery::Replaced { uuid: doc.uuid.clone() }
        }
        existing => {
            // First delivery, or geometry changed under real ink: a new
            // document. Never rewrite pages someone has written on.
            let uuid = new_uuid();
            let name = if existing.is_some() {
                // Versioned so both documents are tellable apart.
                format!("{visible_name} (更新版)")
            } else {
                visible_name.clone()
            };
            write_pdf(xochitl_dir, &uuid, pdf)?;
            write_content_file(xochitl_dir, &uuid, layout.page_count)?;
            write_metadata(xochitl_dir, &uuid, &name)?;
            reg.books.insert(
                layout.book_id.clone(),
                DeliveredDoc {
                    uuid: uuid.clone(),
                    content_sha256: layout.content_sha256.clone(),
                    visible_name: name,
                    decorations_refreshed_at: 0,
                },
            );
            save_registry(registry_path, &reg)?;
            return Ok(Delivery::Created { uuid });
        }
    };

    // Refreshed/Replaced paths: keep the registry's hash current.
    if let Some(doc) = reg.books.get_mut(&layout.book_id) {
        doc.content_sha256 = layout.content_sha256.clone();
    }
    save_registry(registry_path, &reg)?;
    Ok(delivery)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Grid, build};
    use crate::paginate::paginate;
    use std::path::PathBuf;

    fn temp_dirs(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("rm-weread-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let x = base.join("xochitl");
        fs::create_dir_all(&x).unwrap();
        (x, base.join("docs.json"))
    }

    fn layout_for(text: &str) -> BookLayout {
        let grid = Grid { cols: 10, lines_per_page: 4, ..Grid::default() };
        let chapters = vec![crate::layout::ChapterInput {
            chapter_uid: 1,
            title: "一".into(),
            text: text.into(),
            pages: paginate(text, grid.cols, grid.lines_per_page),
            hot: vec![],
        }];
        build("book9", "书名", "作者", &chapters, grid)
    }

    #[test]
    fn first_delivery_creates_a_document() {
        let (x, r) = temp_dirs("create");
        let l = layout_for("正文一");
        let d = deliver(&x, &r, &l, b"%PDF-fake").unwrap();
        let Delivery::Created { uuid } = &d else { panic!("expected Created, got {d:?}") };
        assert!(x.join(format!("{uuid}.pdf")).exists());
        assert!(x.join(format!("{uuid}.metadata")).exists());
        assert!(x.join(format!("{uuid}.content")).exists());
        let reg = load_registry(&r);
        assert_eq!(&reg.books["book9"].uuid, uuid);
        // uuid is v4-shaped.
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
    }

    #[test]
    fn same_hash_refreshes_in_place() {
        let (x, r) = temp_dirs("refresh");
        let l = layout_for("正文一");
        let Delivery::Created { uuid } = deliver(&x, &r, &l, b"v1").unwrap() else { panic!() };
        let d = deliver(&x, &r, &l, b"v2-new-decorations").unwrap();
        assert_eq!(d, Delivery::Refreshed { uuid: uuid.clone() });
        assert_eq!(fs::read(x.join(format!("{uuid}.pdf"))).unwrap(), b"v2-new-decorations");
    }

    #[test]
    fn changed_hash_without_ink_replaces() {
        let (x, r) = temp_dirs("replace");
        let l1 = layout_for("原来的正文");
        let Delivery::Created { uuid } = deliver(&x, &r, &l1, b"v1").unwrap() else { panic!() };
        let l2 = layout_for("修订后的正文");
        assert_ne!(l1.content_sha256, l2.content_sha256);
        let d = deliver(&x, &r, &l2, b"v2").unwrap();
        assert_eq!(d, Delivery::Replaced { uuid: uuid.clone() });
        // Registry follows the new hash.
        assert_eq!(load_registry(&r).books["book9"].content_sha256, l2.content_sha256);
    }

    #[test]
    fn changed_hash_with_ink_creates_a_versioned_sibling() {
        let (x, r) = temp_dirs("ink");
        let l1 = layout_for("原来的正文");
        let Delivery::Created { uuid: u1 } = deliver(&x, &r, &l1, b"v1").unwrap() else { panic!() };
        // Simulate pen strokes on the delivered document.
        fs::create_dir_all(x.join(&u1)).unwrap();
        fs::write(x.join(&u1).join("page0.rm"), b"ink").unwrap();
        let l2 = layout_for("修订后的正文");
        let Delivery::Created { uuid: u2 } = deliver(&x, &r, &l2, b"v2").unwrap() else {
            panic!("inked doc must not be overwritten")
        };
        assert_ne!(u1, u2);
        // Old document untouched.
        assert_eq!(fs::read(x.join(format!("{u1}.pdf"))).unwrap(), b"v1");
        let reg = load_registry(&r);
        assert_eq!(reg.books["book9"].uuid, u2);
        assert!(reg.books["book9"].visible_name.contains("更新版"));
    }
}
