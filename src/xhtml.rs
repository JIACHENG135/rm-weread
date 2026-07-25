//! Strips a decoded chapter's XHTML down to plain text **while keeping a
//! map back to character offsets in the original XHTML**.
//!
//! That offset map is the whole point of this module, not a bonus:
//! WeRead's underline/review APIs address text by `range` values like
//! `"383-415"`, which are character (rune) indices into the *raw chapter
//! HTML* (see koplugin's docs/weread-annotations-flow.md). Because we do
//! our own layout, we can map "the word the user just circled on screen"
//! back to a source index directly — that's what lets us skip koplugin's
//! whole EPUB-footnote/XPointer contraption (see design.md §"为什么我们
//! 不需要 koplugin 的 EPUB-脚注-拦截那套花活"). Building the map now
//! costs a few dozen lines; retrofitting it later would mean redoing
//! pagination.
//!
//! Deliberately not a real HTML parser: chapter bodies come from one
//! generator (WeRead's own), so tag-skipping plus a handful of entities
//! covers them. If a real parser ever becomes necessary, that's a signal
//! something changed upstream, not a reason to have pre-built one.

/// One contiguous stretch of retained text. `out_*` indexes the plain
/// text this module produces; `src_*` indexes the original XHTML. Both
/// are **character** counts, not bytes — matching WeRead's rune-indexed
/// ranges.
#[derive(Debug, Clone, PartialEq)]
struct Segment {
    out_start: usize,
    out_len: usize,
    src_start: usize,
    src_len: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Text {
    pub text: String,
    segments: Vec<Segment>,
}

impl Text {
    /// Maps a character offset in `text` back to its character offset in
    /// the original XHTML. Offsets inside a segment map proportionally
    /// (1:1 except across a multi-char entity, where the whole entity
    /// collapses onto its start).
    pub fn source_offset(&self, out_offset: usize) -> Option<usize> {
        let idx = self
            .segments
            .partition_point(|s| s.out_start + s.out_len <= out_offset)
            .min(self.segments.len().saturating_sub(1));
        let seg = self.segments.get(idx)?;
        let delta = out_offset.saturating_sub(seg.out_start);
        // An entity (src_len > out_len) has no meaningful interior
        // position — clamp to its start rather than inventing one.
        if seg.src_len == seg.out_len {
            Some(seg.src_start + delta.min(seg.src_len))
        } else {
            Some(seg.src_start)
        }
    }

    pub fn char_len(&self) -> usize {
        self.text.chars().count()
    }
}

/// Elements whose *content* is dropped entirely, not just their tags.
const SKIPPED_ELEMENTS: [&str; 2] = ["script", "style"];

/// Elements that force a paragraph break when they open or close.
const BLOCK_ELEMENTS: [&str; 14] = [
    "p", "div", "br", "h1", "h2", "h3", "h4", "h5", "h6", "li", "tr", "blockquote", "section",
    "article",
];

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        "nbsp" | "#160" => Some('\u{a0}'),
        _ => {
            // Numeric entities: &#123; and &#x1F600;
            let rest = name.strip_prefix('#')?;
            let code = match rest.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => rest.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Extracts a tag's lowercase element name and whether it's a closing tag.
fn parse_tag(tag_body: &str) -> (String, bool) {
    let body = tag_body.trim();
    let is_close = body.starts_with('/');
    let body = body.trim_start_matches('/');
    let name: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    (name, is_close)
}

pub fn to_text(xhtml: &str) -> Text {
    let chars: Vec<char> = xhtml.chars().collect();
    let mut out = String::new();
    let mut segments: Vec<Segment> = Vec::new();
    let mut out_chars = 0usize;
    let mut i = 0usize;
    // Set while inside a <script>/<style> body.
    let mut skip_until: Option<String> = None;
    // Collapses runs of whitespace and suppresses leading blank lines.
    let mut pending_break = false;
    let mut pending_space = false;

    while i < chars.len() {
        let c = chars[i];

        if c == '<' {
            let Some(close_rel) = chars[i..].iter().position(|&c| c == '>') else {
                break; // unterminated tag: ignore the rest
            };
            let tag_body: String = chars[i + 1..i + close_rel].iter().collect();
            let (name, is_close) = parse_tag(&tag_body);

            if let Some(skipped) = &skip_until {
                if is_close && &name == skipped {
                    skip_until = None;
                }
            } else if SKIPPED_ELEMENTS.contains(&name.as_str()) && !is_close {
                skip_until = Some(name);
            } else if BLOCK_ELEMENTS.contains(&name.as_str()) && !out.is_empty() {
                pending_break = true;
            }

            i += close_rel + 1;
            continue;
        }

        if skip_until.is_some() {
            i += 1;
            continue;
        }

        if c.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            i += 1;
            continue;
        }

        // A real character is about to be emitted — settle any deferred
        // break/space first (deferring is what collapses whitespace and
        // avoids trailing blank lines).
        if pending_break {
            out.push('\n');
            out_chars += 1;
            pending_break = false;
            pending_space = false;
        } else if pending_space {
            out.push(' ');
            out_chars += 1;
            pending_space = false;
        }

        let (ch, src_len) = if c == '&' {
            match chars[i..].iter().take(12).position(|&c| c == ';') {
                Some(semi_rel) => {
                    let name: String = chars[i + 1..i + semi_rel].iter().collect();
                    match decode_entity(&name) {
                        Some(decoded) => (decoded, semi_rel + 1),
                        None => (c, 1), // not a recognized entity: keep the bare '&'
                    }
                }
                None => (c, 1),
            }
        } else {
            (c, 1)
        };

        // Extend the previous segment when this char directly follows it
        // in *both* spaces; otherwise start a new one.
        match segments.last_mut() {
            Some(last)
                if last.out_start + last.out_len == out_chars
                    && last.src_start + last.src_len == i
                    && last.src_len == last.out_len
                    && src_len == 1 =>
            {
                last.out_len += 1;
                last.src_len += 1;
            }
            _ => segments.push(Segment {
                out_start: out_chars,
                out_len: 1,
                src_start: i,
                src_len,
            }),
        }

        out.push(ch);
        out_chars += 1;
        i += src_len;
    }

    Text { text: out, segments }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_and_breaks_paragraphs() {
        let t = to_text("<html><body><p>第一段</p><p>第二段</p></body></html>");
        assert_eq!(t.text, "第一段\n第二段");
    }

    #[test]
    fn drops_script_and_style_content() {
        let t = to_text("<p>keep</p><script>var x = 1;</script><style>p{color:red}</style><p>also</p>");
        assert_eq!(t.text, "keep\nalso");
    }

    #[test]
    fn decodes_entities() {
        let t = to_text("<p>a&amp;b&lt;c&gt;d&#65;e</p>");
        assert_eq!(t.text, "a&b<c>dAe");
    }

    #[test]
    fn collapses_whitespace() {
        let t = to_text("<p>hello   \n\t  world</p>");
        assert_eq!(t.text, "hello world");
    }

    #[test]
    fn maps_offsets_back_to_source() {
        let xhtml = "<p>abc</p>";
        let t = to_text(xhtml);
        assert_eq!(t.text, "abc");
        // 'a' is at source char index 3, right after "<p>".
        assert_eq!(t.source_offset(0), Some(3));
        assert_eq!(t.source_offset(1), Some(4));
        assert_eq!(t.source_offset(2), Some(5));
    }

    #[test]
    fn maps_offsets_across_tags_and_entities() {
        //              0123456789...
        let xhtml = "<p>ab</p><p>c&amp;d</p>";
        let t = to_text(xhtml);
        assert_eq!(t.text, "ab\nc&d");
        assert_eq!(t.source_offset(0), Some(3)); // 'a'
        assert_eq!(t.source_offset(1), Some(4)); // 'b'
        assert_eq!(t.source_offset(3), Some(12)); // 'c'
        assert_eq!(t.source_offset(4), Some(13)); // '&' — the entity's start
        assert_eq!(t.source_offset(5), Some(18)); // 'd', after "&amp;"
    }

    #[test]
    fn counts_cjk_by_character_not_byte() {
        let xhtml = "<p>中文字符</p>";
        let t = to_text(xhtml);
        assert_eq!(t.char_len(), 4);
        assert_eq!(t.source_offset(0), Some(3));
        assert_eq!(t.source_offset(3), Some(6));
    }

    #[test]
    fn handles_unterminated_tag_without_panicking() {
        let t = to_text("<p>ok</p><p unterminated");
        assert_eq!(t.text, "ok");
    }

    #[test]
    fn empty_input_is_empty() {
        let t = to_text("");
        assert_eq!(t.text, "");
        assert_eq!(t.source_offset(0), None);
    }
}
