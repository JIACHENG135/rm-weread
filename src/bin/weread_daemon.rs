//! Native half of the WeRead → PDF pipeline (docs/design.md §"PDF 生成
//! 流水线与冻结规则"). The fullscreen QML reader this daemon used to
//! feed is gone; reading now happens in xochitl's own PDF reader, and
//! the daemon's jobs are:
//!
//!   1. generate  — build the current book's PDF (hot underlines burned
//!      in) and deliver it into the document library
//!   2. reviews   — answer the QML popup's "what do people say about
//!      this range" asks
//!   3. refresh   — daily decoration refresh, threshold-gated
//!
//! IPC stays the file-trigger/poll shape rm-agent proved on device
//! (QML's CommandExecutor can only run `/bin/touch` — no shell, so no
//! file *content* from QML). Parameters ride in the touched *filename*:
//!
//!   generate                  -> regenerate the first shelf book
//!   ask_<chapterUid>_<range>_<nonce>
//!                             -> write reviews for that hot range
//!
//! Result files (atomic temp+rename, seq-first so QML can poll):
//!   gen.txt      seq / status ("working"|"done"|"error") / message
//!   reviews.txt  seq / status ("ok"|"error") / quote / count /
//!                author<TAB>content per line

use rm_weread::pipeline::{self, Paths};
use rm_weread::underlines;
use rm_weread::xochitl_doc::{self, Delivery};
use rm_weread::{login, pdfgen, session, shelf};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SESSION_PATH: &str = "/home/root/.local/share/rm-weread/session.json";
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Decoration refresh cadence. The hot-underline set moves on a scale
/// of weeks; daily is already generous (and threshold-gated besides).

fn exthome() -> PathBuf {
    Paths::device().exthome
}

fn take_trigger(name: &str) -> bool {
    let path = exthome().join(name);
    if path.exists() {
        let _ = fs::remove_file(&path);
        true
    } else {
        false
    }
}

fn read_seq(path: &Path) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().next().map(str::to_owned))
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(0)
}

fn write_result(file: &str, status: &str, body_lines: &[String]) {
    let path = exthome().join(file);
    let seq = read_seq(&path) + 1;
    let tmp = exthome().join(format!("{file}.tmp"));
    let write = || -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        writeln!(f, "{seq}")?;
        writeln!(f, "{status}")?;
        for line in body_lines {
            writeln!(f, "{}", line.replace('\n', " "))?;
        }
        drop(f);
        fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        eprintln!("weread: failed to write {file}: {e}");
    }
}

fn gen_status(status: &str, message: &str) {
    write_result("gen.txt", status, &[message.to_string()]);
}

fn shelf_status(status: &str, message: &str) {
    write_result("shelf.txt", status, &[message.to_string()]);
}

/// Scans for `ask_<bookId>_<chapterUid>_<range>_<nonce>` triggers.
///
/// The book id is in the filename because there is no "current book"
/// any more — several generated books can be on the device, and the
/// popup knows which one it is in from the open document's layout.
/// The range id itself contains a '-', so split from the left on '_'
/// and take the pieces whole.
fn take_asks() -> Vec<(String, i64, String)> {
    let mut asks = Vec::new();
    let Ok(entries) = fs::read_dir(exthome()) else { return asks };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("ask_") else { continue };
        let _ = fs::remove_file(entry.path());
        let parts: Vec<&str> = rest.splitn(4, '_').collect();
        if parts.len() < 3 {
            eprintln!("weread: malformed ask trigger: {name}");
            continue;
        }
        let Ok(uid) = parts[1].parse::<i64>() else {
            eprintln!("weread: malformed ask chapterUid: {name}");
            continue;
        };
        asks.push((parts[0].to_string(), uid, parts[2].to_string()));
    }
    asks
}

/// Scans for `hot_<bookId>_<chapterUid>_<nonce>` triggers — the QML
/// overlay asking for the underlines of the chapter you just reached.
fn take_hot_requests() -> Vec<(String, i64)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(exthome()) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("hot_") else { continue };
        let _ = fs::remove_file(entry.path());
        let parts: Vec<&str> = rest.splitn(3, '_').collect();
        if parts.len() < 2 {
            eprintln!("weread: malformed hot trigger: {name}");
            continue;
        }
        match parts[1].parse::<i64>() {
            Ok(uid) => out.push((parts[0].to_string(), uid)),
            Err(_) => eprintln!("weread: malformed hot chapterUid: {name}"),
        }
    }
    out
}

/// Answers one overlay request: fetch that chapter's underlines and
/// write its tap boxes where QML can read them.
///
/// Writes the file even when the fetch fails, holding an empty list, so
/// the overlay stops asking for that chapter on every page turn.
fn answer_hot(
    agent: &ureq::Agent,
    sess: &login::Session,
    paths: &Paths,
    book_id: &str,
    chapter_uid: i64,
) {
    let taps = match with_retry("underlines", || {
        pipeline::hot_for_chapter(agent, &sess.api_key, paths, book_id, chapter_uid)
    }) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("weread: underlines for chapter {chapter_uid} failed: {e}");
            Vec::new()
        }
    };
    let dir = paths.exthome.join("hot");
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("weread: cannot create hot dir: {e}");
        return;
    }
    let json = serde_json::to_string(&taps).unwrap_or_else(|_| "[]".into());
    let tmp = dir.join(format!("{book_id}_{chapter_uid}.json.tmp"));
    let dest = dir.join(format!("{book_id}_{chapter_uid}.json"));
    if fs::write(&tmp, json).and_then(|_| fs::rename(&tmp, &dest)).is_err() {
        eprintln!("weread: cannot write hot file for {chapter_uid}");
        return;
    }
    // Bump a sequence so the overlay knows to re-read without polling
    // the (possibly large) result file itself.
    write_result("hot.txt", "ok", &[format!("{book_id} {chapter_uid} {}", taps.len())]);
}

/// Scans for `gen_<bookId>_<nonce>` triggers from the shelf browser.
fn take_gen_requests() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(exthome()) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("gen_") else { continue };
        let _ = fs::remove_file(entry.path());
        match rest.split_once('_') {
            Some((book_id, _)) if !book_id.is_empty() => out.push(book_id.to_string()),
            _ => eprintln!("weread: malformed gen trigger: {name}"),
        }
    }
    out
}

/// Retries an operation that hits the network. Flaky DNS on shared
/// Wi-Fi has broken real runs more than once; one retry a couple of
/// seconds later has been enough every time.
fn with_retry<T>(
    what: &str,
    mut op: impl FnMut() -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    const ATTEMPTS: usize = 3;
    let mut last: Option<Box<dyn std::error::Error>> = None;
    for attempt in 1..=ATTEMPTS {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("weread: {what} attempt {attempt}/{ATTEMPTS} failed: {e}");
                last = Some(e);
                if attempt < ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| "unknown error".into()))
}

/// Makes sure the "＋ 书架" card exists in the folder and tells QML its
/// uuid, so opening that document pops the shelf browser.
fn ensure_shelf_card(paths: &Paths) {
    let pdf = match pdfgen::shelf_card() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("weread: could not build the shelf card: {e}");
            return;
        }
    };
    match xochitl_doc::deliver_shelf_card(&paths.xochitl_dir, &paths.registry(), &pdf) {
        Ok(uuid) => {
            let path = paths.exthome.join("shelf_doc.txt");
            if fs::read_to_string(&path).map(|s| s.trim() != uuid).unwrap_or(true) {
                let _ = fs::write(&path, &uuid);
            }
        }
        Err(e) => eprintln!("weread: could not deliver the shelf card: {e}"),
    }
}

/// Publishes the shelf for the QML browser: `shelf.json` plus a cached
/// cover thumbnail per book that QML loads straight off disk.
///
/// Cover downloads are best-effort and never fail the listing — a book
/// with no artwork still has to be selectable.
fn publish_shelf(
    agent: &ureq::Agent,
    sess: &login::Session,
    paths: &Paths,
) -> Result<usize, Box<dyn std::error::Error>> {
    let shelf = shelf::sync(agent, &sess.api_key)?;
    let reg = xochitl_doc::load_registry(&paths.registry());
    let covers_dir = paths.exthome.join("covers");
    let _ = fs::create_dir_all(&covers_dir);

    let mut books = Vec::new();
    for b in &shelf.books {
        // The shelf URL is already a list-sized thumbnail, which is
        // exactly what the browser wants; the large variant is fetched
        // separately, into its own cache, only when a book is generated.
        let dest = covers_dir.join(format!("{}.jpg", b.book_id));
        let cover_rel = pipeline::fetch_cover(agent, &dest, &b.cover)
            .map(|_| format!("covers/{}.jpg", b.book_id));
        books.push(serde_json::json!({
            "book_id": b.book_id,
            "title": b.title,
            "author": b.author,
            "cover": cover_rel.unwrap_or_default(),
            "finished": b.finish_reading != 0,
            "generated": reg.books.contains_key(&b.book_id),
        }));
    }

    let json = serde_json::to_string(&serde_json::json!({ "books": books }))?;
    let tmp = paths.exthome.join("shelf.json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(tmp, paths.exthome.join("shelf.json"))?;
    Ok(books.len())
}

/// Generates one book by id. An empty id means "the first book on the
/// shelf", which is what the bare `generate` trigger (and the install
/// smoke test) still uses.
fn generate_book_id(
    agent: &ureq::Agent,
    sess: &mut login::Session,
    paths: &Paths,
    book_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let shelf = shelf::sync(agent, &sess.api_key)?;
    let book = if book_id.is_empty() {
        shelf.books.first()
    } else {
        shelf.books.iter().find(|b| b.book_id == book_id)
    }
    .ok_or("书架上找不到这本书")?;

    println!("weread: generating {} / {}", book.title, book.author);
    let generated = pipeline::generate_book(
        agent,
        sess,
        paths,
        &book.book_id,
        &book.title,
        &book.author,
        &book.cover,
        // Zero underline requests during generation: a whole book's
        // worth in a few minutes is what got this client throttled.
        // Underlines are fetched per chapter while reading instead.
        pipeline::HotPolicy::ReuseKnown,
        |note| gen_status("working", note),
    )?;
    let what = match &generated.delivery {
        Delivery::Created { .. } => "已生成新文档",
        Delivery::Refreshed { .. } => "已刷新划线",
        Delivery::Replaced { .. } => "已更新文档",
    };
    let message = format!(
        "{}《{}》: {} 页",
        what,
        book.title,
        generated.layout.page_count
    );

    // xochitl reads the library once at startup: it keeps no inotify
    // watch on the document directory (checked against /proc/<pid>/fdinfo
    // — the directory's inode is not among the watched ones) and exposes
    // no D-Bus call to rescan; the only sync interface on the bus drives
    // *cloud* sync, and a forged batchFinished signal provokes no reload.
    // So a brand-new document cannot appear without restarting xochitl.
    //
    // The restart is offered as a button rather than performed here: a
    // book takes minutes, the reader may well have moved on to another
    // one, and yanking xochitl out from under them is not something to
    // do unannounced. `done_restart` is what tells the popup to show it.
    if matches!(generated.delivery, Delivery::Created { .. }) {
        gen_status("done_restart", &message);
    } else {
        gen_status("done", &message);
    }
    Ok(message)
}

fn answer_ask(
    agent: &ureq::Agent,
    sess: &login::Session,
    _paths: &Paths,
    cache: &mut HashMap<(String, i64, String), underlines::RangeReviews>,
    book_id: String,
    chapter_uid: i64,
    range: String,
) {
    let key = (book_id.clone(), chapter_uid, range.clone());
    let reviews = match cache.get(&key) {
        Some(r) => r.clone(),
        None => {
            match with_retry("readreviews", || {
                underlines::fetch_reviews(agent, &sess.api_key, &book_id, chapter_uid, &range)
            }) {
                Ok(r) => {
                    cache.insert(key, r.clone());
                    r
                }
                Err(e) => {
                    write_result("reviews.txt", "error", &[format!("拉取评论失败: {e}")]);
                    return;
                }
            }
        }
    };

    let mut lines = vec![reviews.quote.clone(), reviews.total.to_string()];
    for r in reviews.reviews.iter().take(20) {
        lines.push(format!("{}\t{}", r.author.replace('\t', " "), r.content.replace('\t', " ")));
    }
    write_result("reviews.txt", "ok", &lines);
}

fn main() {
    let paths = Paths::device();
    fs::create_dir_all(&paths.exthome).expect("failed to create weread exthome dir");
    for stale in ["generate", "shelf", "restart", "open", "next", "prev", "close"] {
        let _ = fs::remove_file(exthome().join(stale));
    }
    let _ = take_asks();
    let _ = take_gen_requests();
    let _ = take_hot_requests();
    // Underlines are per-reading-session now; a stale set from a
    // previous run would draw lines for a book that may since have been
    // regenerated with different geometry.
    let _ = fs::remove_dir_all(paths.exthome.join("hot"));
    ensure_shelf_card(&paths);
    if !exthome().join("gen.txt").exists() {
        gen_status("done", "(空闲)");
    }

    let agent = login::agent();
    let mut sess = match session::load(Path::new(SESSION_PATH)) {
        Some(s) => {
            println!("weread_daemon: loaded session for {}", s.account.name);
            s
        }
        None => {
            eprintln!("weread_daemon: no session at {SESSION_PATH} — log in first");
            gen_status("error", "(未登录)");
            // Keep running: a session may appear later without a restart.
            login::Session {
                cookies: Default::default(),
                api_key: String::new(),
                account: login::Account { name: String::new(), user_vid: String::new() },
            }
        }
    };

    let mut review_cache: HashMap<(String, i64, String), underlines::RangeReviews> = HashMap::new();

    println!("weread_daemon: watching {} for triggers...", paths.exthome.display());
    loop {
        if take_trigger("restart") {
            println!("weread: restarting xochitl so a new document appears");
            let _ = std::process::Command::new("systemctl").arg("restart").arg("xochitl").status();
        }

        if take_trigger("shelf") {
            shelf_status("working", "正在读取书架…");
            match with_retry("shelf", || publish_shelf(&agent, &sess, &paths)) {
                Ok(n) => shelf_status("done", &format!("{n} 本")),
                Err(e) => {
                    eprintln!("weread: shelf sync failed: {e}");
                    shelf_status("error", &format!("读取书架失败: {e}"));
                }
            }
        }

        // A bare `generate` still means "the first book on the shelf" —
        // that's what the install smoke test pokes, with no UI involved.
        let mut wanted: Vec<String> = take_gen_requests();
        if take_trigger("generate") {
            wanted.push(String::new());
        }
        for book_id in wanted {
            gen_status("working", "正在生成…");
            match with_retry("generate", || generate_book_id(&agent, &mut sess, &paths, &book_id)) {
                Ok(_) => {
                    // generate_book_id already published the final
                    // status, including whether a restart is needed.
                    review_cache.clear(); // ranges may have changed
                    let _ = session::save(Path::new(SESSION_PATH), &sess);
                    // The browser shows a "已生成" badge; keep it honest.
                    let _ = publish_shelf(&agent, &sess, &paths);
                }
                Err(e) => {
                    eprintln!("weread: generate failed: {e}");
                    gen_status("error", &format!("生成失败: {e}"));
                }
            }
        }

        for (book_id, chapter_uid) in take_hot_requests() {
            answer_hot(&agent, &sess, &paths, &book_id, chapter_uid);
        }

        for (book_id, chapter_uid, range) in take_asks() {
            answer_ask(&agent, &sess, &paths, &mut review_cache, book_id, chapter_uid, range);
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
