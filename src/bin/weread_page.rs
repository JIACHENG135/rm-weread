//! Fetches a real chapter and lays it out into pages — the end-to-end
//! check that xhtml.rs + paginate.rs handle genuine WeRead prose, not
//! just the synthetic strings in their unit tests.
//!
//! Usage: cargo run --bin weread_page -- [--book-id <id>] [--chapter <n>] [--page <n>]

use rm_weread::{login, paginate, reader, session, shelf, xhtml};
use std::path::PathBuf;

// A rough rM2/Paper Pro portrait text area: ~32 CJK glyphs wide, 26 lines.
const WIDTH_COLUMNS: usize = 64;
const LINES_PER_PAGE: usize = 26;

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
    }
    None
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(arg("--session-path").unwrap_or_else(|| "session.json".into()));
    let mut session: login::Session =
        session::load(&path).ok_or("no session found — run weread_login first")?;
    let agent = login::agent();

    let book_id = match arg("--book-id") {
        Some(id) => id,
        None => {
            let shelf = shelf::sync(&agent, &session.api_key)?;
            let book = shelf.books.first().ok_or("shelf is empty")?;
            println!("book: {} / {}", book.title, book.author);
            book.book_id.clone()
        }
    };

    reader::renew_session(&agent, &mut session.cookies)?;
    let chapters = reader::fetch_chapters(&agent, &mut session.cookies, &book_id)?;
    let idx: usize = arg("--chapter").and_then(|s| s.parse().ok()).unwrap_or(0);
    let chapter = chapters.get(idx).ok_or("chapter index out of range")?;
    println!("chapter {}: {} (uid={})", idx, chapter.title, chapter.chapter_uid);

    let bytes = reader::fetch_chapter_content(&agent, &mut session.cookies, &book_id, chapter)?;
    let raw = String::from_utf8_lossy(&bytes);

    let text = xhtml::to_text(&raw);
    println!(
        "source XHTML: {} chars -> plain text: {} chars",
        raw.chars().count(),
        text.char_len()
    );

    let pages = paginate::paginate(&text.text, WIDTH_COLUMNS, LINES_PER_PAGE);
    println!("{} page(s) at {WIDTH_COLUMNS} columns x {LINES_PER_PAGE} lines\n", pages.len());

    let page_no: usize = arg("--page").and_then(|s| s.parse().ok()).unwrap_or(0);
    let page = pages.get(page_no).ok_or("page index out of range")?;
    println!("--- page {page_no} (chars {}..{}) ---", page.start, page.end);
    for line in &page.lines {
        println!("{line}");
    }
    println!("--- end of page ---");

    // Verifies the two invariants phases 6/7 depend on, on every page —
    // a screen position has to resolve to the right plain-text offset,
    // and that offset has to resolve to the right raw-HTML index.
    let mut mismatches = 0;
    for (n, p) in pages.iter().enumerate() {
        let Some(first_line) = p.lines.first().filter(|l| !l.is_empty()) else { continue };
        let probe: String = first_line.chars().take(8).collect();

        let from_text: String = text.text.chars().skip(p.start).take(probe.chars().count()).collect();
        if from_text != probe {
            println!("page {n}: plain-text offset mismatch: {from_text:?} != {probe:?}");
            mismatches += 1;
            continue;
        }

        // The source index should land on the same character, though the
        // raw HTML around it still carries tags.
        match text.source_offset(p.start) {
            Some(src) => {
                let src_char = raw.chars().nth(src);
                let want = probe.chars().next();
                if src_char != want {
                    println!("page {n}: source offset {src} is {src_char:?}, expected {want:?}");
                    mismatches += 1;
                }
            }
            None => {
                println!("page {n}: no source offset for start {}", p.start);
                mismatches += 1;
            }
        }
    }
    println!(
        "\noffset round-trip over all {} page(s): {}",
        pages.len(),
        if mismatches == 0 { "OK".to_string() } else { format!("{mismatches} MISMATCH(ES)") }
    );

    session::save(&path, &session)?;
    Ok(())
}
