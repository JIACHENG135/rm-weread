//! Hot underlines and their reviews, via the Skill Gateway
//! (docs/design.md §"关键 API 端点一览"): `/book/underlines` for the
//! heat map (range + count, no text) and `/book/readreviews` for the
//! thoughts attached to a range.
//!
//! Contracts verified against a real account (2026-07-25, real
//! captures; the defensive-parse fallbacks are kept for shape drift):
//!
//! `/book/underlines` → `{synckey, bookId, chapterUid, underlines:
//! [{count, score, type, range}]}`. `count` has been observed as 0
//! across the board while `score` (0..1 float) carries the heat — so
//! heat below is `max(count, score*1000)`.
//!
//! `/book/readreviews` requires a `reviews` ARRAY in the request:
//! `{bookId, chapterUid, synckey: 0, reviews: [{range, maxIdx: 0,
//! count: N, synckey: 0}]}` — `maxIdx` must be 0 or `pageReviews`
//! comes back empty (real observation, not documented anywhere).
//! Response: `{reviews: [{range, totalCount, hasMore, pageReviews:
//! [{review: {content, abstract, author: {name, nick}}}]}]}`.
//!
//! `range` values are rune offsets into the *raw chapter XHTML*
//! (koplugin docs), so callers must map them through
//! `xhtml::Text::out_offset_for_source` before they mean anything to
//! our layout. `map_to_text` does exactly that.

use crate::skill_gateway;
use crate::xhtml;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Underline {
    /// WeRead's range id, verbatim ("345-361").
    pub range: String,
    /// Parsed endpoints of `range`: source-XHTML char offsets,
    /// inclusive start, inclusive end (WeRead's convention).
    pub src_start: usize,
    pub src_end: usize,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Review {
    pub author: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RangeReviews {
    pub quote: String,
    pub total: u32,
    pub reviews: Vec<Review>,
}

fn parse_range(range: &str) -> Option<(usize, usize)> {
    let (a, b) = range.split_once('-')?;
    let start: usize = a.trim().parse().ok()?;
    let end: usize = b.trim().parse().ok()?;
    (start <= end).then_some((start, end))
}

/// Finds the first array of objects under any of the usual wrapper keys
/// the gateway has been seen using (`data`, `updated`, `items`, or the
/// value itself).
fn unwrap_items(payload: &Value) -> Vec<&Value> {
    for key in ["items", "updated", "data", "underlines", "bestBookmarks"] {
        if let Some(arr) = payload.get(key).and_then(Value::as_array) {
            return arr.iter().collect();
        }
        // One level of nesting ({"data": {"items": [...]}}) shows up in
        // several gateway responses.
        if let Some(inner) = payload.get(key)
            && !inner.is_array()
            && let Some(arr) = ["items", "updated", "underlines"]
                .iter()
                .find_map(|k| inner.get(k).and_then(Value::as_array))
        {
            return arr.iter().collect();
        }
    }
    payload.as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

fn str_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(k).and_then(Value::as_str))
}

fn u32_field(v: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|k| v.get(k).and_then(Value::as_u64)).map(|n| n as u32)
}

fn i64_field(v: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|k| {
        let val = v.get(k)?;
        val.as_i64().or_else(|| val.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Parses an `/book/underlines`-shaped payload down to the ranges of
/// one chapter. Pure so it can be pinned by tests and re-pinned against
/// real captures later.
pub fn parse_underlines(payload: &Value, chapter_uid: i64) -> Vec<Underline> {
    let mut out = Vec::new();
    for item in unwrap_items(payload) {
        if let Some(uid) = i64_field(item, &["chapterUid", "chapterUID", "chapter_uid"])
            && uid != chapter_uid
        {
            continue;
        }
        let Some(range) = str_field(item, &["range", "markRange"]) else { continue };
        let Some((src_start, src_end)) = parse_range(range) else { continue };
        let count = u32_field(item, &["count", "totalCount", "markCount", "userCount"]).unwrap_or(0);
        // Real captures: `count` is 0 for every entry and the heat is
        // in `score` (0..1). Fold score into a pseudo-count on the same
        // scale the underline style tiers use.
        let score = item.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let heat = count.max((score * 1000.0).round() as u32).max(1);
        out.push(Underline { range: range.to_string(), src_start, src_end, count: heat });
    }
    out
}

/// Parses a `/book/readreviews`-shaped payload for one range. The real
/// shape nests everything under `reviews[0]` (we only ever ask for one
/// range); the generic fallbacks below cover drift.
pub fn parse_reviews(payload: &Value) -> RangeReviews {
    let mut out = RangeReviews::default();
    if let Some(q) = str_field(payload, &["quote", "abstract", "markText"]) {
        out.quote = q.to_string();
    }
    // Real contract: {"reviews": [{totalCount, pageReviews: [...]}]}.
    let range_obj = payload.get("reviews").and_then(|r| r.get(0));
    let effective = range_obj.unwrap_or(payload);
    let items = match effective.get("pageReviews").and_then(Value::as_array) {
        Some(arr) => arr.iter().collect(),
        None => unwrap_items(payload),
    };
    for item in &items {
        // Reviews commonly nest the actual review under "review".
        let r = item.get("review").unwrap_or(item);
        let content = str_field(r, &["content", "text"]).unwrap_or_default();
        if content.is_empty() {
            continue;
        }
        let author = r
            .get("author")
            .and_then(|a| str_field(a, &["name", "nick", "nickName"]))
            .or_else(|| str_field(r, &["authorName", "name", "nick"]))
            .unwrap_or("匿名")
            .to_string();
        if out.quote.is_empty()
            && let Some(q) = str_field(r, &["abstract", "quote", "markText"])
        {
            out.quote = q.to_string();
        }
        out.reviews.push(Review { author, content: content.to_string() });
    }
    out.total = u32_field(effective, &["totalCount", "total", "reviewCount"])
        .or_else(|| u32_field(payload, &["totalCount", "total", "reviewCount"]))
        .unwrap_or(out.reviews.len() as u32);
    out
}

pub fn fetch_underlines(
    agent: &ureq::Agent,
    api_key: &str,
    book_id: &str,
    chapter_uid: i64,
) -> Result<Vec<Underline>, Box<dyn std::error::Error>> {
    let payload: Value = skill_gateway::call(
        agent,
        api_key,
        "/book/underlines",
        serde_json::json!({ "bookId": book_id, "chapterUid": chapter_uid }),
    )?;
    Ok(parse_underlines(&payload, chapter_uid))
}

pub fn fetch_reviews(
    agent: &ureq::Agent,
    api_key: &str,
    book_id: &str,
    chapter_uid: i64,
    range: &str,
) -> Result<RangeReviews, Box<dyn std::error::Error>> {
    // Real contract (see module docs): `reviews` must be an array of
    // range requests, and `maxIdx` must be 0 or `pageReviews` comes
    // back empty. `synckey: 0` works fine (no sync state needed).
    let payload: Value = skill_gateway::call(
        agent,
        api_key,
        "/book/readreviews",
        serde_json::json!({
            "bookId": book_id,
            "chapterUid": chapter_uid,
            "synckey": 0,
            "reviews": [{ "range": range, "maxIdx": 0, "count": 20, "synckey": 0 }],
        }),
    )?;
    Ok(parse_reviews(&payload))
}

/// Maps source-XHTML underline ranges onto plain-text offsets for the
/// layout. Ranges that don't intersect any retained text are dropped.
pub fn map_to_text(underlines: &[Underline], text: &xhtml::Text) -> Vec<crate::layout::HotInput> {
    let chars: Vec<char> = text.text.chars().collect();
    let mut out = Vec::new();
    for u in underlines {
        let Some(start) = text.out_offset_for_source(u.src_start) else { continue };
        // `src_end` maps to the first character *past* the run, not the
        // last one in it. Checked against WeRead's own `abstract` for
        // range 621-670 of 球状闪电: treating it as inclusive and adding
        // one drew two characters too many, and the second of them was
        // the first character of the *next paragraph*.
        let Some(end) = text.out_offset_for_source(u.src_end) else { continue };
        if end <= start {
            continue;
        }
        // `to_text` inserts paragraph breaks that have no counterpart in
        // the source, so a mapped end can land just past one. An
        // underline that runs through a line break draws a stray cell at
        // the end of the line.
        let mut end = end.min(chars.len());
        while end > start && chars[end - 1].is_whitespace() {
            end -= 1;
        }
        if end <= start {
            continue;
        }
        out.push(crate::layout::HotInput {
            range: u.range.clone(),
            off: start,
            len: end - start,
            count: u.count,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_underlines_and_filters_by_chapter() {
        let payload = json!({
            "data": {
                "items": [
                    { "chapterUid": 3, "range": "10-20", "count": 42 },
                    { "chapterUid": 4, "range": "5-9", "count": 7 },
                    { "chapterUid": 3, "range": "junk" },
                    { "chapterUid": 3, "markRange": "30-31", "totalCount": 9 }
                ]
            }
        });
        let got = parse_underlines(&payload, 3);
        assert_eq!(
            got,
            vec![
                Underline { range: "10-20".into(), src_start: 10, src_end: 20, count: 42 },
                Underline { range: "30-31".into(), src_start: 30, src_end: 31, count: 9 },
            ]
        );
    }

    #[test]
    fn underline_items_without_chapter_uid_are_kept() {
        // When we asked for one chapter, an item without the field is
        // assumed to belong to it rather than dropped.
        let payload = json!([{ "range": "1-2" }]);
        assert_eq!(parse_underlines(&payload, 5).len(), 1);
    }

    #[test]
    fn parses_reviews_with_nested_author() {
        let payload = json!({
            "totalCount": 231,
            "quote": "被划的原文",
            "items": [
                { "review": { "content": "想法一", "author": { "name": "读者甲" } } },
                { "content": "想法二", "authorName": "读者乙" },
                { "content": "" }
            ]
        });
        let got = parse_reviews(&payload);
        assert_eq!(got.quote, "被划的原文");
        assert_eq!(got.total, 231);
        assert_eq!(
            got.reviews,
            vec![
                Review { author: "读者甲".into(), content: "想法一".into() },
                Review { author: "读者乙".into(), content: "想法二".into() },
            ]
        );
    }

    #[test]
    fn parses_real_underlines_capture_with_score_heat() {
        // Verbatim shape from a real /book/underlines response
        // (2026-07-25): count is 0, heat lives in `score`.
        let payload = json!({
            "synckey": 1785018653u64,
            "bookId": "25504039",
            "chapterUid": 1,
            "underlines": [
                { "count": 0, "score": 0.5009508728981018, "type": 0, "range": "154-171" },
                { "count": 0, "score": 0.7231466174125671, "type": 0, "range": "244-277" },
                { "count": 0, "score": 0.00025042882771231234, "type": 0, "range": "420-422" }
            ]
        });
        let got = parse_underlines(&payload, 1);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].count, 501);
        assert_eq!(got[1].count, 723);
        assert_eq!(got[2].count, 1); // floor of 1 so it still draws
        assert_eq!(got[1].range, "244-277");
    }

    #[test]
    fn parses_real_readreviews_capture() {
        // Trimmed but structurally verbatim real /book/readreviews
        // response (2026-07-25).
        let payload = json!({
            "reviews": [{
                "range": "244-277",
                "bookMarkCount": 0.0,
                "maxIdx": 0,
                "totalCount": 3,
                "hasMore": 0,
                "synckey": 1785018653u64,
                "pageReviews": [{
                    "reviewId": "5739010_89DNq20AV",
                    "review": {
                        "bookId": "25504039",
                        "content": "有点夸张了，让我来看看怎么个事儿。",
                        "range": "244-277",
                        "abstract": "《球状闪电》是他对人生的终极思考。",
                        "chapterUid": 1,
                        "author": { "userVid": 5739010, "name": "巴斯托尼", "nick": "巴斯托尼" }
                    }
                }]
            }],
            "bookId": "25504039",
            "chapterUid": 1
        });
        let got = parse_reviews(&payload);
        assert_eq!(got.total, 3);
        assert_eq!(got.reviews.len(), 1);
        assert_eq!(got.reviews[0].author, "巴斯托尼");
        assert_eq!(got.reviews[0].content, "有点夸张了，让我来看看怎么个事儿。");
        assert_eq!(got.quote, "《球状闪电》是他对人生的终极思考。");
    }

    #[test]
    fn maps_source_ranges_to_text_offsets() {
        let t = crate::xhtml::to_text("<p>abcde</p>");
        // 'a' is at source 3, and WeRead's end is exclusive: "3-5"
        // covers "ab". Checked against the `abstract` the review API
        // returns for a real range rather than assumed — reading it as
        // inclusive drew a character too many on every underline.
        let hot = map_to_text(
            &[Underline { range: "3-5".into(), src_start: 3, src_end: 5, count: 2 }],
            &t,
        );
        assert_eq!(hot.len(), 1);
        assert_eq!((hot[0].off, hot[0].len, hot[0].count), (0, 2, 2));
    }

    #[test]
    fn an_underline_never_runs_through_a_paragraph_break() {
        // to_text inserts a break between paragraphs that has no source
        // counterpart, so a mapped end can land past it — and then the
        // run trails a blank cell and swallows the next paragraph's
        // first character. This is what made underlines look one
        // character too long on a real page.
        let t = crate::xhtml::to_text("<p>ab</p><p>cd</p>");
        assert_eq!(t.text, "ab
cd");
        let src_end = t.text.len(); // safely past "ab" and the break
        let hot = map_to_text(
            &[Underline { range: "r".into(), src_start: 3, src_end, count: 1 }],
            &t,
        );
        let h = &hot[0];
        let covered: String = t.text.chars().skip(h.off).take(h.len).collect();
        assert!(!covered.ends_with(char::is_whitespace), "trailing break in {covered:?}");
        assert!(covered.starts_with("ab"));
    }

    #[test]
    fn a_range_that_maps_to_nothing_is_dropped() {
        let t = crate::xhtml::to_text("<p>ab</p>");
        // Zero-width and inverted ranges must not become underlines.
        assert!(map_to_text(&[Underline { range: "z".into(), src_start: 3, src_end: 3, count: 1 }], &t).is_empty());
        assert!(map_to_text(&[Underline { range: "z".into(), src_start: 5, src_end: 3, count: 1 }], &t).is_empty());
    }

    #[test]
    fn drops_ranges_outside_retained_text() {
        let t = crate::xhtml::to_text("<p>ab</p>");
        let hot = map_to_text(
            &[Underline { range: "100-120".into(), src_start: 100, src_end: 120, count: 1 }],
            &t,
        );
        assert!(hot.is_empty());
    }
}
