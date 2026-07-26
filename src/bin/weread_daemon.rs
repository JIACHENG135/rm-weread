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
use rm_weread::{login, session, shelf};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SESSION_PATH: &str = "/home/root/.local/share/rm-weread/session.json";
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Decoration refresh cadence. The hot-underline set moves on a scale
/// of weeks; daily is already generous (and threshold-gated besides).
const REFRESH_EVERY_SECS: u64 = 24 * 60 * 60;

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

/// Scans for `ask_<chapterUid>_<range>_<nonce>` trigger files. The
/// range id itself contains a '-', so split from the left on '_' and
/// take the middle piece whole.
fn take_asks() -> Vec<(i64, String)> {
    let mut asks = Vec::new();
    let Ok(entries) = fs::read_dir(exthome()) else { return asks };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("ask_") else { continue };
        let _ = fs::remove_file(entry.path());
        let parts: Vec<&str> = rest.splitn(3, '_').collect();
        if parts.len() < 2 {
            eprintln!("weread: malformed ask trigger: {name}");
            continue;
        }
        let Ok(uid) = parts[0].parse::<i64>() else {
            eprintln!("weread: malformed ask chapterUid: {name}");
            continue;
        };
        asks.push((uid, parts[1].to_string()));
    }
    asks
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

fn generate_first_shelf_book(
    agent: &ureq::Agent,
    sess: &mut login::Session,
    paths: &Paths,
) -> Result<String, Box<dyn std::error::Error>> {
    let shelf = shelf::sync(agent, &sess.api_key)?;
    let book = shelf.books.first().ok_or("书架是空的")?;
    println!("weread: generating {} / {}", book.title, book.author);
    let generated = pipeline::generate_book(
        agent,
        sess,
        paths,
        &book.book_id,
        &book.title,
        &book.author,
        |note| gen_status("working", note),
    )?;
    let what = match &generated.delivery {
        Delivery::Created { .. } => "已生成新文档",
        Delivery::Refreshed { .. } => "已刷新划线",
        Delivery::Replaced { .. } => "已更新文档",
    };
    Ok(format!(
        "{}《{}》: {} 页, {} 处热门划线。如果文档没出现，重启 xochitl 一次。",
        what,
        book.title,
        generated.layout.page_count,
        generated.layout.hot_count()
    ))
}

fn answer_ask(
    agent: &ureq::Agent,
    sess: &login::Session,
    paths: &Paths,
    cache: &mut HashMap<(i64, String), underlines::RangeReviews>,
    chapter_uid: i64,
    range: String,
) {
    // book_id comes from the current layout.json — the popup and this
    // daemon share the same "current book" slot (multi-book needs the
    // current-document lookup; see docs/design.md's on-device TODO).
    let book_id = fs::read_to_string(paths.exthome.join("layout.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("book_id").and_then(|b| b.as_str()).map(str::to_owned));
    let Some(book_id) = book_id else {
        write_result("reviews.txt", "error", &["还没有生成任何书".to_string()]);
        return;
    };

    let key = (chapter_uid, range.clone());
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

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Threshold-gated daily refresh for every delivered book.
fn maybe_refresh(agent: &ureq::Agent, sess: &mut login::Session, paths: &Paths) {
    let registry_path = paths.registry();
    let mut reg = xochitl_doc::load_registry(&registry_path);
    let now = now_secs();
    let mut dirty = false;
    let book_ids: Vec<String> = reg.books.keys().cloned().collect();
    for book_id in book_ids {
        let due = reg
            .books
            .get(&book_id)
            .map(|d| now.saturating_sub(d.decorations_refreshed_at) >= REFRESH_EVERY_SECS)
            .unwrap_or(false);
        if !due {
            continue;
        }
        match pipeline::refresh_decorations(agent, sess, paths, &book_id) {
            Ok(rebuilt) => {
                println!(
                    "weread: decoration refresh for {book_id}: {}",
                    if rebuilt { "rebuilt" } else { "below threshold, skipped" }
                );
            }
            Err(e) => eprintln!("weread: decoration refresh for {book_id} failed: {e}"),
        }
        // Checked (successfully or not) — don't retry until tomorrow.
        if let Some(d) = reg.books.get_mut(&book_id) {
            d.decorations_refreshed_at = now;
            dirty = true;
        }
    }
    if dirty {
        let _ = xochitl_doc::save_registry(&registry_path, &reg);
    }
}

fn main() {
    let paths = Paths::device();
    fs::create_dir_all(&paths.exthome).expect("failed to create weread exthome dir");
    for stale in ["generate", "open", "next", "prev", "close"] {
        let _ = fs::remove_file(exthome().join(stale));
    }
    let _ = take_asks();
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

    let mut review_cache: HashMap<(i64, String), underlines::RangeReviews> = HashMap::new();
    let mut last_refresh_check = 0u64;

    println!("weread_daemon: watching {} for triggers...", paths.exthome.display());
    loop {
        if take_trigger("generate") {
            gen_status("working", "正在生成…");
            match with_retry("generate", || generate_first_shelf_book(&agent, &mut sess, &paths)) {
                Ok(message) => {
                    gen_status("done", &message);
                    review_cache.clear(); // ranges may have changed
                    let _ = session::save(Path::new(SESSION_PATH), &sess);
                }
                Err(e) => {
                    eprintln!("weread: generate failed: {e}");
                    gen_status("error", &format!("生成失败: {e}"));
                }
            }
        }

        for (chapter_uid, range) in take_asks() {
            answer_ask(&agent, &sess, &paths, &mut review_cache, chapter_uid, range);
        }

        // Cheap hourly check of the daily refresh clock.
        let now = now_secs();
        if now.saturating_sub(last_refresh_check) >= 3600 {
            last_refresh_check = now;
            maybe_refresh(&agent, &mut sess, &paths);
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
