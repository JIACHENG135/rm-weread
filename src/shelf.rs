//! `/shelf/sync` — the user's bookshelf. Feature 1 (书架) reads straight
//! from here; nothing is stored server-side by us, this is always a live
//! call against WeRead's own account state (see docs/design.md's "数据
//! 落地" section).

use crate::skill_gateway;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Book {
    #[serde(rename = "bookId")]
    pub book_id: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub category: String,
    #[serde(rename = "readUpdateTime", default)]
    pub read_update_time: u64,
    #[serde(rename = "finishReading", default)]
    pub finish_reading: u32,
}

#[derive(Debug, Deserialize)]
pub struct Shelf {
    // No `bookCount` field from the server — confirmed on a real
    // account's response (it's simply absent, not zero). The API
    // reference's documented formula counts audiobook albums and the
    // MP-articles pseudo-entry too, but our reader only handles books,
    // so callers should just use `books.len()`.
    #[serde(default)]
    pub books: Vec<Book>,
}

pub fn sync(agent: &ureq::Agent, api_key: &str) -> Result<Shelf, Box<dyn std::error::Error>> {
    skill_gateway::call(agent, api_key, "/shelf/sync", serde_json::Value::Null)
}
