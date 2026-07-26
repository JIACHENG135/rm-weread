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

/// Library folder every generated book is delivered into, so they don't
/// scatter through the user's own documents.
pub const FOLDER_NAME: &str = "微信读书";

/// The card that opens the shelf browser. Named so it sorts and reads
/// like an action, not like a book.
pub const SHELF_CARD_NAME: &str = "＋ 书架";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    /// book_id → delivered document.
    pub books: BTreeMap<String, DeliveredDoc>,
    /// uuid of the `微信读书` collection. Remembered so renaming or
    /// moving the folder on-device doesn't make us create a second one.
    #[serde(default)]
    pub folder_uuid: Option<String>,
    /// uuid of the one-page "书架" card that opens the shelf browser.
    #[serde(default)]
    pub shelf_doc_uuid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveredDoc {
    pub uuid: String,
    pub content_sha256: String,
    pub visible_name: String,
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

/// True when a delivered document is still in the library.
///
/// Deleting on this firmware means `parent: "trash"` — the metadata
/// file stays put and `deleted` stays false, so presence on disk proves
/// nothing. Without this check the shelf browser kept showing a book as
/// "on the device" after the reader had thrown it away, and a rebuild
/// would have tried to refresh a document sitting in the trash.
pub fn document_is_live(xochitl_dir: &Path, uuid: &str) -> bool {
    let Ok(raw) = fs::read_to_string(xochitl_dir.join(format!("{uuid}.metadata"))) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    v.get("deleted").and_then(|d| d.as_bool()) != Some(true)
        && v.get("parent").and_then(|p| p.as_str()) != Some("trash")
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

/// Finds or creates the `微信读书` collection and returns its uuid.
///
/// Order matters: trust the registry first (the user may have renamed
/// the folder, and it's still theirs), then look for a folder already
/// carrying the name (a reinstall that lost `docs.json` must not create
/// a duplicate), and only then make one. A collection is metadata only
/// — no `.content`, no payload file.
fn ensure_folder(xochitl_dir: &Path, reg: &mut Registry) -> std::io::Result<String> {
    if let Some(uuid) = &reg.folder_uuid
        && xochitl_dir.join(format!("{uuid}.metadata")).exists()
    {
        return Ok(uuid.clone());
    }

    if let Some(uuid) = find_collection(xochitl_dir, FOLDER_NAME) {
        reg.folder_uuid = Some(uuid.clone());
        return Ok(uuid);
    }

    let uuid = new_uuid();
    let metadata = serde_json::json!({
        "visibleName": FOLDER_NAME,
        "type": "CollectionType",
        "parent": "",
        "createdTime": now_ms().to_string(),
        "lastModified": now_ms().to_string(),
        "new": false,
        "pinned": false,
        "source": "",
    });
    fs::write(
        xochitl_dir.join(format!("{uuid}.metadata")),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )?;
    reg.folder_uuid = Some(uuid.clone());
    Ok(uuid)
}

/// Scans the library for a non-deleted collection with this name.
fn find_collection(xochitl_dir: &Path, name: &str) -> Option<String> {
    let mut found: Vec<String> = fs::read_dir(xochitl_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "metadata").unwrap_or(false))
        .filter_map(|e| {
            let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok()?;
            let is_match = v.get("type").and_then(|t| t.as_str()) == Some("CollectionType")
                && v.get("visibleName").and_then(|n| n.as_str()) == Some(name)
                && v.get("deleted").and_then(|d| d.as_bool()) != Some(true);
            is_match.then(|| e.path().file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    // Deterministic pick if the user somehow has two folders of the
    // same name, so we don't ping-pong between them run to run.
    found.sort();
    found.into_iter().next()
}

/// Delivers the one-page "书架" card into the folder and returns its
/// uuid, creating it only once.
///
/// It is left alone on later runs: it is a fixed page the reader may
/// have scribbled on, and rewriting it would buy nothing.
pub fn deliver_shelf_card(
    xochitl_dir: &Path,
    registry_path: &Path,
    pdf: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
    fs::create_dir_all(xochitl_dir)?;
    let mut reg = load_registry(registry_path);
    let folder = ensure_folder(xochitl_dir, &mut reg)?;

    if let Some(uuid) = &reg.shelf_doc_uuid
        && xochitl_dir.join(format!("{uuid}.pdf")).exists()
    {
        let uuid = uuid.clone();
        save_registry(registry_path, &reg)?;
        return Ok(uuid);
    }

    let uuid = new_uuid();
    write_pdf(xochitl_dir, &uuid, pdf)?;
    write_content_file(xochitl_dir, &uuid, 1)?;
    write_metadata(xochitl_dir, &uuid, SHELF_CARD_NAME, &folder)?;
    reg.shelf_doc_uuid = Some(uuid.clone());
    save_registry(registry_path, &reg)?;
    Ok(uuid)
}

/// Documents delivered before the folder existed carry a "— 微信读书"
/// suffix that the folder now says for them; drop it so they don't read
/// as "书名 — 微信读书" sitting inside a folder called 微信读书. Not a
/// suffix strip: the versioned form is "书名 — 微信读书 (更新版)", so
/// the marker has to come out of the middle too.
fn migrated_name(stored: &str) -> String {
    stored.replace(" — 微信读书", "")
}

fn write_metadata(xochitl_dir: &Path, uuid: &str, visible_name: &str, parent: &str) -> std::io::Result<()> {
    let path = xochitl_dir.join(format!("{uuid}.metadata"));
    // A refresh rewrites this file to nudge xochitl's change detection,
    // and it must not take the reader's state with it. Reading position
    // and pinning belong to the user, not to the generator — for the
    // same reason the freeze rules protect their ink.
    let old: serde_json::Value = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let keep = |key: &str, fallback: serde_json::Value| {
        old.get(key).cloned().unwrap_or(fallback)
    };

    let metadata = serde_json::json!({
        "visibleName": visible_name,
        "type": "DocumentType",
        "parent": parent,
        "lastModified": now_ms().to_string(),
        "lastOpened": keep("lastOpened", "".into()),
        "lastOpenedPage": keep("lastOpenedPage", 0.into()),
        "createdTime": keep("createdTime", now_ms().to_string().into()),
        "pinned": keep("pinned", false.into()),
        "version": 1,
        "synced": false,
        "modified": false,
        "deleted": false,
        "metadatamodified": false
    });
    fs::write(&path, serde_json::to_string_pretty(&metadata).unwrap())
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
    let folder = ensure_folder(xochitl_dir, &mut reg)?;
    // Bare title: the folder already says where these came from, so the
    // old "— 微信读书" suffix would just repeat it in every row.
    let visible_name = layout.title.clone();

    // A book whose document the reader deleted starts over: refreshing
    // or "replacing" something in the trash would resurrect it in place
    // and leave them with a document they threw away.
    if let Some(doc) = reg.books.get(&layout.book_id)
        && !document_is_live(xochitl_dir, &doc.uuid)
    {
        reg.books.remove(&layout.book_id);
    }

    let delivery = match reg.books.get(&layout.book_id) {
        Some(doc) if doc.content_sha256 == layout.content_sha256 => {
            // Geometry unchanged: decoration-only refresh, ink is safe.
            write_pdf(xochitl_dir, &doc.uuid, pdf)?;
            drop_thumbnails(xochitl_dir, &doc.uuid);
            // Nudge xochitl's change detection.
            write_metadata(xochitl_dir, &doc.uuid, &migrated_name(&doc.visible_name), &folder)?;
            Delivery::Refreshed { uuid: doc.uuid.clone() }
        }
        Some(doc) if !has_ink(xochitl_dir, &doc.uuid) => {
            // Geometry changed but nothing is anchored to it yet.
            write_pdf(xochitl_dir, &doc.uuid, pdf)?;
            write_content_file(xochitl_dir, &doc.uuid, layout.page_count)?;
            write_metadata(xochitl_dir, &doc.uuid, &visible_name, &folder)?;
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
            write_metadata(xochitl_dir, &uuid, &name, &folder)?;
            reg.books.insert(
                layout.book_id.clone(),
                DeliveredDoc {
                    uuid: uuid.clone(),
                    content_sha256: layout.content_sha256.clone(),
                    visible_name: name,
                },
            );
            save_registry(registry_path, &reg)?;
            return Ok(Delivery::Created { uuid });
        }
    };

    // Refreshed/Replaced paths: keep the registry's hash current, and
    // let the migrated name stick so the rename happens exactly once.
    if let Some(doc) = reg.books.get_mut(&layout.book_id) {
        doc.content_sha256 = layout.content_sha256.clone();
        doc.visible_name = migrated_name(&doc.visible_name);
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
        let grid = Grid { text_em: 5000, lines_per_page: 4, ..Grid::default() };
        let chapters = vec![crate::layout::ChapterInput {
            chapter_uid: 1,
            title: "一".into(),
            text: text.into(),
            pages: paginate(text, grid.text_em, grid.lines_per_page),
            hot: vec![],
        }];
        build("book9", "书名", "作者", &chapters, grid, false)
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

    fn meta(x: &Path, uuid: &str) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(x.join(format!("{uuid}.metadata"))).unwrap()).unwrap()
    }

    fn collections(x: &Path) -> Vec<String> {
        fs::read_dir(x)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map(|s| s == "metadata").unwrap_or(false))
            .filter(|e| {
                let v: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(e.path()).unwrap()).unwrap();
                v["type"] == "CollectionType"
            })
            .map(|e| e.path().file_stem().unwrap().to_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn delivery_lands_in_the_weread_folder_under_a_bare_title() {
        let (x, r) = temp_dirs("folder");
        let l = layout_for("正文一");
        let Delivery::Created { uuid } = deliver(&x, &r, &l, b"v1").unwrap() else { panic!() };

        let folders = collections(&x);
        assert_eq!(folders.len(), 1);
        assert_eq!(meta(&x, &folders[0])["visibleName"], FOLDER_NAME);
        assert_eq!(meta(&x, &folders[0])["type"], "CollectionType");
        // A collection is metadata only — no payload, no .content.
        assert!(!x.join(format!("{}.content", folders[0])).exists());

        assert_eq!(meta(&x, &uuid)["parent"], folders[0]);
        // The folder carries "微信读书", so the document must not.
        assert_eq!(meta(&x, &uuid)["visibleName"], "书名");
        assert_eq!(load_registry(&r).folder_uuid.unwrap(), folders[0]);
    }

    #[test]
    fn an_existing_folder_is_adopted_rather_than_duplicated() {
        // A reinstall that lost docs.json must not leave the user with
        // two identically named folders.
        let (x, r) = temp_dirs("adopt");
        let existing = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        fs::write(
            x.join(format!("{existing}.metadata")),
            serde_json::to_string(&serde_json::json!({
                "visibleName": FOLDER_NAME, "type": "CollectionType", "parent": "", "deleted": false
            }))
            .unwrap(),
        )
        .unwrap();

        let l = layout_for("正文一");
        let Delivery::Created { uuid } = deliver(&x, &r, &l, b"v1").unwrap() else { panic!() };
        assert_eq!(collections(&x), vec![existing.to_string()]);
        assert_eq!(meta(&x, &uuid)["parent"], existing);
    }

    #[test]
    fn a_deleted_folder_is_not_adopted() {
        let (x, r) = temp_dirs("deleted-folder");
        fs::write(
            x.join("dddddddd-bbbb-4ccc-8ddd-eeeeeeeeeeee.metadata"),
            serde_json::to_string(&serde_json::json!({
                "visibleName": FOLDER_NAME, "type": "CollectionType", "parent": "", "deleted": true
            }))
            .unwrap(),
        )
        .unwrap();

        let l = layout_for("正文一");
        deliver(&x, &r, &l, b"v1").unwrap();
        // The tombstone is left alone and a live folder made alongside.
        assert_eq!(collections(&x).len(), 2);
        let live = load_registry(&r).folder_uuid.unwrap();
        assert_ne!(live, "dddddddd-bbbb-4ccc-8ddd-eeeeeeeeeeee");
    }

    #[test]
    fn a_legacy_suffixed_name_is_migrated_once_on_refresh() {
        let (x, r) = temp_dirs("rename");
        let l = layout_for("正文一");
        let Delivery::Created { uuid } = deliver(&x, &r, &l, b"v1").unwrap() else { panic!() };

        // Pretend this document predates the folder, in both spellings
        // the old naming could produce.
        let mut reg = load_registry(&r);
        reg.books.get_mut("book9").unwrap().visible_name = "书名 — 微信读书 (更新版)".into();
        save_registry(&r, &reg).unwrap();

        deliver(&x, &r, &l, b"v2").unwrap();
        assert_eq!(meta(&x, &uuid)["visibleName"], "书名 (更新版)");
        assert_eq!(load_registry(&r).books["book9"].visible_name, "书名 (更新版)");
    }

    #[test]
    fn a_refresh_keeps_the_readers_place_and_pin() {
        let (x, r) = temp_dirs("keep-position");
        let l = layout_for("正文一");
        let Delivery::Created { uuid } = deliver(&x, &r, &l, b"v1").unwrap() else { panic!() };

        // Simulate the user reading to page 42 and pinning the book.
        let mut m = meta(&x, &uuid);
        m["lastOpened"] = "1785026414316".into();
        m["lastOpenedPage"] = 42.into();
        m["pinned"] = true.into();
        let created = m["createdTime"].clone();
        fs::write(
            x.join(format!("{uuid}.metadata")),
            serde_json::to_string_pretty(&m).unwrap(),
        )
        .unwrap();

        // A decoration refresh rewrites the metadata; it must not cost
        // the reader their position.
        assert_eq!(deliver(&x, &r, &l, b"v2").unwrap(), Delivery::Refreshed { uuid: uuid.clone() });
        let after = meta(&x, &uuid);
        assert_eq!(after["lastOpened"], "1785026414316");
        assert_eq!(after["lastOpenedPage"], 42);
        assert_eq!(after["pinned"], true);
        assert_eq!(after["createdTime"], created);
    }

    #[test]
    fn the_shelf_card_lands_in_the_folder_and_is_created_once() {
        let (x, r) = temp_dirs("shelf-card");
        let uuid = deliver_shelf_card(&x, &r, b"%PDF-card").unwrap();
        let folder = load_registry(&r).folder_uuid.unwrap();
        assert_eq!(meta(&x, &uuid)["parent"], folder);
        assert_eq!(meta(&x, &uuid)["visibleName"], SHELF_CARD_NAME);
        assert!(x.join(format!("{uuid}.content")).exists());

        // Stable across runs: the reader may have written on it, and a
        // changing uuid would break the QML trigger.
        let again = deliver_shelf_card(&x, &r, b"%PDF-different").unwrap();
        assert_eq!(again, uuid);
        assert_eq!(fs::read(x.join(format!("{uuid}.pdf"))).unwrap(), b"%PDF-card");

        // And it shares the folder with generated books.
        let l = layout_for("正文一");
        let Delivery::Created { uuid: book } = deliver(&x, &r, &l, b"v1").unwrap() else { panic!() };
        assert_eq!(meta(&x, &book)["parent"], folder);
        assert_eq!(collections(&x).len(), 1);
    }

    #[test]
    fn a_trashed_document_stops_counting_as_delivered() {
        let (x, r) = temp_dirs("trashed");
        let l = layout_for("正文一");
        let Delivery::Created { uuid: first } = deliver(&x, &r, &l, b"v1").unwrap() else { panic!() };
        assert!(document_is_live(&x, &first));

        // Deleting on device means parent: "trash"; the metadata file
        // stays and `deleted` stays false.
        let mut m = meta(&x, &first);
        m["parent"] = "trash".into();
        fs::write(
            x.join(format!("{first}.metadata")),
            serde_json::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
        assert!(!document_is_live(&x, &first));

        // Same content, but it must not refresh the trashed document —
        // that would resurrect something the reader threw away.
        let d = deliver(&x, &r, &l, b"v2").unwrap();
        let Delivery::Created { uuid: second } = d else { panic!("expected Created, got {d:?}") };
        assert_ne!(second, first);
        assert_eq!(load_registry(&r).books["book9"].uuid, second);
        // And the name is clean: this is a first delivery, not "(更新版)".
        assert_eq!(meta(&x, &second)["visibleName"], "书名");
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
