//! Pulls the bits we need out of a WeRead reader page's embedded
//! `window.__INITIAL_STATE__` blob. Ported from koplugin's
//! `lib/reader_state.lua` — but only its *fallback* path: koplugin tries
//! a full JSON decode of the extracted state object first and falls back
//! to direct regex-style field matching only if that fails. We go
//! straight to the field-matching path (no dependency on the blob being
//! strict, parseable JSON — it's server-templated JS, not guaranteed
//! JSON — and no regex crate needed for a handful of fixed `"key":"..."`
//! lookups).

fn extract_string_field(html: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = html.find(&needle)? + needle.len();
    let end = html[start..].find('"')? + start;
    Some(html[start..end].to_string())
}

fn extract_numeric_or_string_field(html: &str, key: &str) -> Option<String> {
    if let Some(v) = extract_string_field(html, key) {
        return Some(v);
    }
    let needle = format!("\"{key}\":");
    let start = html.find(&needle)? + needle.len();
    let end = html[start..].find(|c: char| !c.is_ascii_digit())?;
    if end == 0 {
        return None;
    }
    Some(html[start..start + end].to_string())
}

#[derive(Debug, Default)]
pub struct ReaderState {
    pub book_id: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub psvts: Option<String>,
    pub pclts: Option<String>,
    pub token: Option<String>,
}

pub fn extract(html: &str) -> ReaderState {
    ReaderState {
        book_id: extract_numeric_or_string_field(html, "bookId"),
        title: extract_string_field(html, "title"),
        author: extract_string_field(html, "author"),
        psvts: extract_string_field(html, "psvts"),
        pclts: extract_string_field(html, "pclts"),
        token: extract_string_field(html, "token"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fields_from_a_minimal_state_blob() {
        let html = r#"<script>window.__INITIAL_STATE__ = {"reader":{"bookInfo":{"bookId":"907755","title":"天行健","author":"燕垒生"},"psvts":"abc123","pclts":"def456","token":"tok789"}};(function(){})()</script>"#;
        let state = extract(html);
        assert_eq!(state.book_id.as_deref(), Some("907755"));
        assert_eq!(state.title.as_deref(), Some("天行健"));
        assert_eq!(state.author.as_deref(), Some("燕垒生"));
        assert_eq!(state.psvts.as_deref(), Some("abc123"));
        assert_eq!(state.pclts.as_deref(), Some("def456"));
        assert_eq!(state.token.as_deref(), Some("tok789"));
    }

    #[test]
    fn falls_back_to_bare_numeric_book_id() {
        let html = r#""bookId":907755,"other":1"#;
        assert_eq!(extract(html).book_id.as_deref(), Some("907755"));
    }

    #[test]
    fn missing_fields_are_none() {
        let state = extract("no state here");
        assert!(state.psvts.is_none());
    }
}
