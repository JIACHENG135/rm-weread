//! Native half of the WeRead reader. The QML side (xovi/weread.qmd) only
//! touches trigger files and polls a result file — the exact same IPC
//! shape rm-agent's translate_daemon/vocab_daemon already proved on
//! device, chosen because QML's CommandExecutor can only run
//! `/bin/touch` (no shell, so no way to write content from QML).
//!
//! Triggers (all touch-only, consumed on read):
//!   open   -> open the first shelf book at its first chapter
//!   next   -> next page (rolls into the next chapter at the end)
//!   prev   -> previous page (rolls back into the previous chapter)
//!   close  -> drop cached state
//!
//! Result file `page.txt` is written atomically (temp + rename) so the
//! polling QML never reads a half-written file:
//!   seq
//!   status      "page" | "error"
//!   title       chapter title
//!   pageinfo    e.g. "3/10"
//!   body...     already-wrapped lines, rendered verbatim by QML

use rm_weread::{login, paginate, reader, session, shelf, xhtml};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const EXTHOME: &str = "/home/root/xovi/exthome/weread";
const SESSION_PATH: &str = "/home/root/.local/share/rm-weread/session.json";
const POLL_INTERVAL: Duration = Duration::from_millis(250);

// Layout, in the character-grid terms paginate.rs works in (a CJK glyph
// is 2 columns). Tuned against xovi/weread.qmd's font.pixelSize and
// margins on a Paper Pro; a screen with different metrics needs these
// re-tuned, which is why they're named constants rather than inline.
const WIDTH_COLUMNS: usize = 84;
const LINES_PER_PAGE: usize = 34;

struct Book {
    id: String,
    chapters: Vec<reader::Chapter>,
}

struct OpenChapter {
    index: usize,
    title: String,
    pages: Vec<paginate::Page>,
    page: usize,
}

fn trigger_path(name: &str) -> PathBuf {
    Path::new(EXTHOME).join(name)
}

/// Consumes a touch-only trigger file, reporting whether it was present.
fn take_trigger(name: &str) -> bool {
    let path = trigger_path(name);
    if path.exists() {
        let _ = fs::remove_file(&path);
        true
    } else {
        false
    }
}

fn read_current_seq() -> u64 {
    fs::read_to_string(trigger_path("page.txt"))
        .ok()
        .and_then(|s| s.lines().next().map(str::to_owned))
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(0)
}

fn write_result(status: &str, title: &str, page_info: &str, body: &str) -> std::io::Result<()> {
    let seq = read_current_seq() + 1;
    let path = trigger_path("page.txt");
    let tmp = trigger_path("page.txt.tmp");
    let mut f = fs::File::create(&tmp)?;
    writeln!(f, "{seq}")?;
    writeln!(f, "{status}")?;
    writeln!(f, "{}", title.replace('\n', " "))?;
    writeln!(f, "{page_info}")?;
    write!(f, "{body}")?;
    drop(f);
    fs::rename(&tmp, &path)
}

fn show(chapter: &OpenChapter) {
    let body = match chapter.pages.get(chapter.page) {
        Some(p) => p.lines.join("\n"),
        None => String::new(),
    };
    let info = format!("{} / {}", chapter.page + 1, chapter.pages.len().max(1));
    if let Err(e) = write_result("page", &chapter.title, &info, &body) {
        eprintln!("weread: failed to write result: {e}");
    }
}

fn show_error(message: &str) {
    if let Err(e) = write_result("error", "", "", message) {
        eprintln!("weread: failed to write error result: {e}");
    }
}

/// Fetches + paginates one chapter. `at_end` starts on the last page,
/// so paging backwards into a chapter lands where the reader expects.
fn load_chapter(
    agent: &ureq::Agent,
    sess: &mut login::Session,
    book: &Book,
    index: usize,
    at_end: bool,
) -> Result<OpenChapter, Box<dyn std::error::Error>> {
    let chapter = book.chapters.get(index).ok_or("chapter index out of range")?;
    let bytes = reader::fetch_chapter_content(agent, &mut sess.cookies, &book.id, chapter)?;
    let text = xhtml::to_text(&String::from_utf8_lossy(&bytes));
    let pages = paginate::paginate(&text.text, WIDTH_COLUMNS, LINES_PER_PAGE);
    let page = if at_end { pages.len().saturating_sub(1) } else { 0 };
    Ok(OpenChapter { index, title: chapter.title.clone(), pages, page })
}

fn open_book(
    agent: &ureq::Agent,
    sess: &mut login::Session,
) -> Result<(Book, OpenChapter), Box<dyn std::error::Error>> {
    let shelf = shelf::sync(agent, &sess.api_key)?;
    let first = shelf.books.first().ok_or("书架是空的")?;
    println!("weread: opening {} / {}", first.title, first.author);
    reader::renew_session(agent, &mut sess.cookies)?;
    let chapters = reader::fetch_chapters(agent, &mut sess.cookies, &first.book_id)?;
    let book = Book { id: first.book_id.clone(), chapters };
    let chapter = load_chapter(agent, sess, &book, 0, false)?;
    Ok((book, chapter))
}

fn main() {
    fs::create_dir_all(EXTHOME).expect("failed to create weread exthome dir");
    // Make sure QML's very first poll finds a well-formed file.
    if !trigger_path("page.txt").exists() {
        let _ = write_result("error", "", "", "(尚未打开)");
    }
    for t in ["open", "next", "prev", "close"] {
        let _ = fs::remove_file(trigger_path(t));
    }

    let agent = login::agent();
    let mut sess = match session::load(Path::new(SESSION_PATH)) {
        Some(s) => {
            println!("weread_daemon: loaded session for {}", s.account.name);
            s
        }
        None => {
            eprintln!("weread_daemon: no session at {SESSION_PATH} — log in first");
            show_error("(未登录)");
            // Keep running: a session may appear later without a restart.
            login::Session {
                cookies: Default::default(),
                api_key: String::new(),
                account: login::Account { name: String::new(), user_vid: String::new() },
            }
        }
    };

    let mut book: Option<Book> = None;
    let mut current: Option<OpenChapter> = None;

    println!("weread_daemon: watching {EXTHOME} for triggers...");
    loop {
        if take_trigger("close") {
            book = None;
            current = None;
        }

        if take_trigger("open") {
            show_error("正在打开…");
            match open_book(&agent, &mut sess) {
                Ok((b, c)) => {
                    book = Some(b);
                    show(&c);
                    current = Some(c);
                    let _ = session::save(Path::new(SESSION_PATH), &sess);
                }
                Err(e) => {
                    eprintln!("weread: open failed: {e}");
                    show_error(&format!("(打开失败: {e})"));
                }
            }
        }

        let step = if take_trigger("next") {
            Some(1i64)
        } else if take_trigger("prev") {
            Some(-1i64)
        } else {
            None
        };

        if let (Some(step), Some(b), Some(c)) = (step, book.as_ref(), current.as_mut()) {
            let next_page = c.page as i64 + step;
            if next_page >= 0 && (next_page as usize) < c.pages.len() {
                c.page = next_page as usize;
                show(c);
            } else {
                // Past either end of the chapter — roll into the
                // neighbouring one, landing on the page the reader is
                // moving toward.
                let next_chapter = c.index as i64 + step;
                if next_chapter < 0 || next_chapter as usize >= b.chapters.len() {
                    show(c); // already at the very start/end of the book
                } else {
                    show_error("正在加载…");
                    match load_chapter(&agent, &mut sess, b, next_chapter as usize, step < 0) {
                        Ok(loaded) => {
                            show(&loaded);
                            *c = loaded;
                        }
                        Err(e) => {
                            eprintln!("weread: chapter load failed: {e}");
                            show_error(&format!("(加载失败: {e})"));
                        }
                    }
                }
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
