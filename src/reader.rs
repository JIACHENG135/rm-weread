//! Web reader flow: chapter catalog, chapter content (signed, cookie
//! -authenticated — this is *not* the Bearer-authenticated Skill Gateway
//! `skill_gateway.rs` talks to) and content decode via `content.rs`.
//! Ported from koplugin's `lib/content.lua`
//! (`fetch_catalog`/`ensure_reader_state`/`fetch_chapter_shard`/
//! `fetch_chapter_xhtml`) and `lib/weread.lua` (`reader_url`,
//! `make_content_params`).

use crate::cookie::CookieJar;
use crate::{content, reader_state, weread_sign};
use serde::Deserialize;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

const CHAPTER_INFOS_URL: &str = "https://weread.qq.com/web/book/chapterInfos";
const RENEWAL_URL: &str = "https://weread.qq.com/web/login/renewal";

#[derive(Debug, Clone, Deserialize)]
pub struct Chapter {
    #[serde(rename = "chapterUid")]
    pub chapter_uid: i64,
    #[serde(rename = "chapterIdx", default)]
    pub chapter_idx: i64,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "wordCount", default)]
    pub word_count: i64,
}

pub fn reader_url(book_id: &str, chapter_uid: Option<i64>) -> String {
    let mut url = format!("https://weread.qq.com/web/reader/{}", weread_sign::e_hash(book_id));
    if let Some(uid) = chapter_uid {
        url.push('k');
        url.push_str(&weread_sign::e_hash(&uid.to_string()));
    }
    url
}

/// Refreshes the Web session — koplugin's docs say to call this before
/// any content request; persist whatever cookies come back.
pub fn renew_session(agent: &ureq::Agent, cookies: &mut CookieJar) -> Result<(), Box<dyn std::error::Error>> {
    let resp = agent
        .post(RENEWAL_URL)
        .header("Content-Type", "application/json;charset=UTF-8")
        .header("Origin", "https://weread.qq.com")
        .header("Referer", "https://weread.qq.com/")
        .header("Cookie", cookies.to_header())
        .send_json(json!({"rq": "%2Fweb%2Fbook%2Fread", "ql": false}))?;
    cookies.merge_from_response(&resp);
    Ok(())
}

/// Fetches and parses `window.__INITIAL_STATE__` off a reader page —
/// needed for `psvts`, which every signed content request requires.
pub fn fetch_reader_state(
    agent: &ureq::Agent,
    cookies: &mut CookieJar,
    reader_url: &str,
) -> Result<reader_state::ReaderState, Box<dyn std::error::Error>> {
    let mut resp = agent
        .get(reader_url)
        .header("Referer", reader_url)
        .header("Cookie", cookies.to_header())
        .call()?;
    cookies.merge_from_response(&resp);
    let html = resp.body_mut().read_to_string()?;
    let state = reader_state::extract(&html);
    if state.psvts.is_none() {
        return Err("reader.psvts not found in reader page".into());
    }
    Ok(state)
}

/// Ported from `Content.normalize_chapters` + `Content.readable_chapters`:
/// unwraps whichever of `{data:...}` / a bare record / an array of
/// records the response turned out to be, finds the record matching
/// `book_id`, and filters out zero-word-count entries and the "封面"
/// (cover) placeholder chapter.
fn parse_chapters(payload: &serde_json::Value, book_id: &str) -> Vec<Chapter> {
    let records = payload.get("data").unwrap_or(payload);
    let records_array: Vec<&serde_json::Value> = if records.get("bookId").is_some() || records.get("updated").is_some() {
        vec![records]
    } else if let Some(arr) = records.as_array() {
        arr.iter().collect()
    } else {
        Vec::new()
    };

    for record in records_array {
        let record_book_id = record.get("bookId").map(|v| v.to_string().trim_matches('"').to_string());
        if record_book_id.as_deref() != Some(book_id) {
            continue;
        }
        let chapters_json = record
            .get("updated")
            .or_else(|| record.get("chapterInfos"))
            .or_else(|| record.get("chapters"))
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));
        let chapters: Vec<Chapter> = serde_json::from_value(chapters_json).unwrap_or_default();
        return chapters.into_iter().filter(|c| c.word_count > 0 && c.title != "封面").collect();
    }
    Vec::new()
}

pub fn fetch_chapters(
    agent: &ureq::Agent,
    cookies: &mut CookieJar,
    book_id: &str,
) -> Result<Vec<Chapter>, Box<dyn std::error::Error>> {
    let referer = reader_url(book_id, None);
    let mut resp = agent
        .post(CHAPTER_INFOS_URL)
        .header("Content-Type", "application/json;charset=UTF-8")
        .header("Origin", "https://weread.qq.com")
        .header("Referer", &referer)
        .header("Cookie", cookies.to_header())
        .send_json(json!({"bookIds": [book_id]}))?;
    cookies.merge_from_response(&resp);
    let payload: serde_json::Value = resp.body_mut().read_json()?;
    Ok(parse_chapters(&payload, book_id))
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Ported from `WeRead.make_content_params`.
fn make_content_params(book_id: &str, chapter_uid: i64, psvts: &str, style: bool) -> serde_json::Value {
    let mut ct = now_secs();
    let pc_candidate = weread_sign::e_hash(&ct.to_string());
    if pc_candidate == psvts {
        ct += 1;
    }
    let pc = weread_sign::e_hash(&ct.to_string());
    // Lua: tostring(math.random(0, 9999) ^ 2) — `^` always returns a
    // float, so this must carry the same ".0" suffix a real request
    // does (see weread_sign.rs's module docs). Computed as plain integer
    // arithmetic + a literal ".0" appended, rather than through an f64,
    // to sidestep Rust's f64 Display never printing that suffix itself
    // for whole numbers (`(4.0f64).to_string()` is `"4"`, not `"4.0"`).
    let n = rand_0_9999() as u64;
    let r = format!("{}.0", n * n);

    let params: Vec<(&str, weread_sign::Param)> = vec![
        ("b", weread_sign::Param::Str(weread_sign::e_hash(book_id))),
        ("c", weread_sign::Param::Str(weread_sign::e_hash(&chapter_uid.to_string()))),
        ("r", weread_sign::Param::Str(r.clone())),
        ("ct", weread_sign::Param::Str(ct.to_string())),
        ("ps", weread_sign::Param::Str(psvts.to_string())),
        ("pc", weread_sign::Param::Str(pc.clone())),
        ("sc", weread_sign::Param::Int(1)),
        ("prevChapter", weread_sign::Param::Bool(false)),
        ("st", weread_sign::Param::Int(if style { 1 } else { 0 })),
    ];
    let s = weread_sign::sign(&weread_sign::sorted_query(&params));

    json!({
        "b": weread_sign::e_hash(book_id),
        "c": weread_sign::e_hash(&chapter_uid.to_string()),
        "r": r,
        "ct": ct.to_string(),
        "ps": psvts,
        "pc": pc,
        "sc": 1,
        "prevChapter": false,
        "st": if style { 1 } else { 0 },
        "s": s,
    })
}

/// `math.random(0, 9999)` — not cryptographic, just a request nonce.
fn rand_0_9999() -> u32 {
    let mut bytes = [0u8; 2];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes)).is_err() {
        return 42; // /dev/urandom unavailable is not a realistic case on our targets; any fixed nonce still works
    }
    (u16::from_le_bytes(bytes) as u32) % 10000
}

fn fetch_chapter_shard(
    agent: &ureq::Agent,
    cookies: &mut CookieJar,
    book_id: &str,
    chapter_uid: i64,
    psvts: &str,
    endpoint: &str,
    style: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let chapter_url = reader_url(book_id, Some(chapter_uid));
    let params = make_content_params(book_id, chapter_uid, psvts, style);
    let mut resp = agent
        .post(format!("https://weread.qq.com{endpoint}"))
        .header("Content-Type", "application/json;charset=UTF-8")
        .header("Origin", "https://weread.qq.com")
        .header("Referer", &chapter_url)
        .header("Cookie", cookies.to_header())
        .send_json(params)?;
    cookies.merge_from_response(&resp);
    let text = resp.body_mut().read_to_string()?;
    if text == "{}" {
        return Err(format!("{endpoint} returned empty object (wrong signature, expired session, or no entitlement)").into());
    }
    Ok(text)
}

/// Fetches and decodes one chapter's body. Handles the EPUB (`e_0`/
/// `e_1`/`e_3`) vs TXT (`t_0`/`t_1`) format split by probing `e_0` first,
/// same as koplugin's `fetch_chapter_xhtml` — a TXT-format book's `e_0`
/// comes back as chapter-metadata JSON (starts with `{` and contains
/// `"bookId"`) instead of encoded content.
pub fn fetch_chapter_content(
    agent: &ureq::Agent,
    cookies: &mut CookieJar,
    book_id: &str,
    chapter: &Chapter,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let reader_url = reader_url(book_id, Some(chapter.chapter_uid));
    let state = fetch_reader_state(agent, cookies, &reader_url)?;
    let psvts = state.psvts.ok_or("missing psvts after fetch_reader_state")?;

    let e0 = fetch_chapter_shard(agent, cookies, book_id, chapter.chapter_uid, &psvts, "/web/book/chapter/e_0", false)?;
    if e0.starts_with('{') && e0.contains("\"bookId\"") {
        let t0 = fetch_chapter_shard(agent, cookies, book_id, chapter.chapter_uid, &psvts, "/web/book/chapter/t_0", false)?;
        let t1 = fetch_chapter_shard(agent, cookies, book_id, chapter.chapter_uid, &psvts, "/web/book/chapter/t_1", false)
            .unwrap_or_default();
        return Ok(content::decode_content_shards(&t0, &t1, "")?);
    }

    let e1 = fetch_chapter_shard(agent, cookies, book_id, chapter.chapter_uid, &psvts, "/web/book/chapter/e_1", false)?;
    let e3 = fetch_chapter_shard(agent, cookies, book_id, chapter.chapter_uid, &psvts, "/web/book/chapter/e_3", false)?;
    Ok(content::decode_content_shards(&e0, &e1, &e3)?)
}
