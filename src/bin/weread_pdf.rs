//! Runs the full PDF pipeline against a real account from a desktop —
//! the same end-to-end validation weread_chapter/weread_page gave
//! earlier phases, extended to layout + PDF + (fake) delivery. Nothing
//! here touches a real xochitl dir unless you point it at one.
//!
//! Usage: cargo run --bin weread_pdf -- [--session-path session.json]
//!            [--book-id <id>] [--out-dir out] [--chapters <n>]

use rm_weread::pipeline::{self, Paths};
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
    let path = PathBuf::from(arg("--session-path").unwrap_or_else(|| "session.json".into()));
    let mut sess: login::Session =
        session::load(&path).ok_or("no session found — run weread_login first")?;
    let agent = login::agent();

    let out_dir = PathBuf::from(arg("--out-dir").unwrap_or_else(|| "out".into()));
    let paths = Paths {
        data_dir: out_dir.join("data"),
        xochitl_dir: out_dir.join("xochitl"),
        exthome: out_dir.join("exthome"),
    };

    let (book_id, title, author) = match arg("--book-id") {
        Some(id) => (id, "未知书名".to_string(), String::new()),
        None => {
            let shelf = shelf::sync(&agent, &sess.api_key)?;
            let book = shelf.books.first().ok_or("shelf is empty")?;
            (book.book_id.clone(), book.title.clone(), book.author.clone())
        }
    };
    println!("book: {title} / {author} ({book_id})");

    // Optional cap for quick runs: only the first N chapters.
    if let Some(n) = arg("--chapters").and_then(|s| s.parse::<usize>().ok()) {
        // The pipeline caps nothing itself; emulate by pre-fetching the
        // catalog and warning how big the run will be.
        reader::renew_session(&agent, &mut sess.cookies)?;
        let total = reader::fetch_chapters(&agent, &mut sess.cookies, &book_id)?.len();
        if n < total {
            println!("note: --chapters {n} requested but the pipeline generates whole books ({total} chapters) — freezing partial geometry would poison later ink. Generating all.");
        }
    }

    let generated = pipeline::generate_book(&agent, &mut sess, &paths, &book_id, &title, &author, |note| {
        println!("  {note}");
    })?;
    session::save(&path, &sess)?;

    println!(
        "delivered: {:?}\npages: {}, markers: {}\nPDF + metadata in {}\nlayout.json in {}",
        generated.delivery,
        generated.layout.page_count,
        generated.layout.markers.len(),
        paths.xochitl_dir.display(),
        paths.exthome.display(),
    );
    Ok(())
}
