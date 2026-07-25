//! Phase 3 test binary: fetches and decodes one real chapter from a real
//! account's shelf — the actual proof that content.rs's decode and
//! weread_sign.rs's signing work against genuine WeRead responses, not
//! just the synthetic Lua-derived test vectors in their unit tests.
//!
//! Usage: cargo run --bin weread_chapter [-- --session-path <path>] [--book-id <id>]
//! Requires an existing session.json from weread_login. Defaults to the
//! first book on the shelf if --book-id isn't given.

use rm_weread::{login, reader, session, shelf};
use std::path::PathBuf;

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
    let path = arg("--session-path").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("session.json"));
    let mut session: login::Session =
        session::load(&path).ok_or("no session found — run weread_login first")?;

    let agent = login::agent();

    let book_id = match arg("--book-id") {
        Some(id) => id,
        None => {
            println!("no --book-id given, using the first book on the shelf...");
            let shelf = shelf::sync(&agent, &session.api_key)?;
            let book = shelf.books.first().ok_or("shelf is empty")?;
            println!("using: {} / {} ({})", book.title, book.author, book.book_id);
            book.book_id.clone()
        }
    };

    println!("renewing web session...");
    reader::renew_session(&agent, &mut session.cookies)?;

    println!("fetching chapter catalog...");
    let chapters = reader::fetch_chapters(&agent, &mut session.cookies, &book_id)?;
    println!("{} readable chapter(s)", chapters.len());
    let chapter = chapters.first().ok_or("no readable chapters")?;
    println!("fetching + decoding: {} (uid={})", chapter.title, chapter.chapter_uid);

    let bytes = reader::fetch_chapter_content(&agent, &mut session.cookies, &book_id, chapter)?;
    let text = String::from_utf8_lossy(&bytes);
    println!("decoded {} bytes, {} chars", bytes.len(), text.chars().count());
    println!("--- first 500 chars ---");
    println!("{}", text.chars().take(500).collect::<String>());

    session::save(&path, &session)?;
    Ok(())
}
