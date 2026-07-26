//! Fixed-layout PDF generation — the output end of the pipeline
//! (docs/design.md §"PDF 生成流水线与冻结规则").
//!
//! Hand-written PDF, deliberately: the layout is a uniform character
//! grid whose geometry must match layout.rs *exactly* (ink anchors to
//! it), so every glyph is placed at its grid column via TJ kerning
//! adjustments rather than the font's natural advances. A general PDF
//! library buys nothing here and costs control over determinism — the
//! same inputs must produce byte-identical output so a decoration
//! refresh (new underlines, same text) can be diffed and trusted. No
//! timestamps, no random IDs.
//!
//! Font: Noto Sans CJK SC (TrueType outlines, subset to the CJK +
//! punctuation ranges at build time — see assets/), embedded whole as a
//! CIDFontType2/Identity-H program. CID == GID (`/CIDToGIDMap
//! /Identity`), so the cmap lookup happens here at generation time and
//! the PDF itself stays trivial for xochitl's renderer. Per-book glyph
//! subsetting is a possible later size optimization; the flate-
//! compressed full subset is a few MB per book, which the device can
//! afford (noted as a TODO, not a blocker).
//!
//! What gets drawn, per page: running chapter title in the top margin,
//! body lines on the grid, hot underlines (dashed and gray, weight and
//! dash density mapped from heat — they are also the tap targets the
//! QML popup hit-tests via layout.json's `taps`), and a page number in
//! the bottom margin. Decorations live in the margins or under the text
//! so they never move the geometry.

use crate::layout::{self, BookLayout, ChapterInput, Grid};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use ttf_parser::{Face, GlyphId};

/// The CJK font shipped in the repo (see assets/README note in
/// docs/design.md): Noto Sans CJK SC Regular, converted to TrueType
/// outlines and subset to CJK Unified + kana + punctuation + circled
/// digits. OFL-licensed, so redistributing it here is fine.
pub const FONT: &[u8] = include_bytes!("../assets/NotoSansSC-Regular.ttf");

/// Underline stroke tiers by heat: `(width, dash pattern, gray level)`.
///
/// All tiers are dashed and gray rather than solid black — the marks
/// sit under the reader's own ink and should read as a quiet hint, not
/// as part of the text. Heat still shows through, in dash density and
/// weight: hotter is denser and slightly darker. Thresholds only affect
/// decoration, never geometry, so tuning them later is a decoration
/// refresh, not a new document.
fn underline_style(count: u32) -> (f32, &'static str, f32) {
    if count >= 1000 {
        (1.0, "[4 2] 0 d", 0.35)
    } else if count >= 100 {
        (0.8, "[3 3] 0 d", 0.45)
    } else {
        (0.7, "[2 4] 0 d", 0.55)
    }
}

struct FontInfo<'a> {
    face: Face<'a>,
    /// Advance widths in 1/1000 em (PDF text space units).
    scale: f32,
}

impl<'a> FontInfo<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, String> {
        let face = Face::parse(data, 0).map_err(|e| format!("font parse: {e}"))?;
        let scale = 1000.0 / face.units_per_em() as f32;
        Ok(FontInfo { face, scale })
    }

    fn gid(&self, c: char) -> u16 {
        self.face.glyph_index(c).map(|g| g.0).unwrap_or(0)
    }

    fn advance(&self, gid: u16) -> i32 {
        let a = self.face.glyph_hor_advance(GlyphId(gid)).unwrap_or(0);
        (a as f32 * self.scale).round() as i32
    }
}

fn flate(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).expect("in-memory write");
    enc.finish().expect("in-memory flush")
}

/// UTF-16BE hex string with BOM — the escaping-free way to put CJK text
/// into PDF strings (outlines, Info dict).
fn utf16_hex(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        let _ = write!(out, "{u:04X}");
    }
    out.push('>');
    out
}

/// One line of body text as a TJ array: glyph hex runs interleaved with
/// kerning adjustments that force every character onto its grid column
/// (adjustment = natural advance − desired advance, in 1/1000 em; a
/// CJK glyph is 2 columns = 1000 units, so CJK runs need none and
/// compress into plain hex runs). Also records used glyphs.
fn line_tj(font: &FontInfo, text: &str, used: &mut BTreeMap<u16, char>) -> String {
    let mut tj = String::from("[<");
    for c in text.chars() {
        let gid = font.gid(c);
        used.entry(gid).or_insert(c);
        let natural = font.advance(gid);
        let desired = (crate::paginate::char_width(c) * 500) as i32;
        let _ = write!(tj, "{gid:04X}");
        let adj = natural - desired;
        if adj != 0 {
            let _ = write!(tj, ">{adj}<");
        }
    }
    tj.push_str(">] TJ");
    tj
}

/// A single positioned string (headers, footers): no grid, no
/// kerning games — natural advances are fine for decorations.
fn show_text_at(
    font: &FontInfo,
    used: &mut BTreeMap<u16, char>,
    size: f32,
    x: f32,
    y: f32,
    gray: f32,
    text: &str,
) -> String {
    let mut hex = String::from("<");
    for c in text.chars() {
        let gid = font.gid(c);
        used.entry(gid).or_insert(c);
        let _ = write!(hex, "{gid:04X}");
    }
    hex.push('>');
    format!("BT /F1 {size:.1} Tf {gray:.2} g 1 0 0 1 {x:.2} {y:.2} Tm {hex} Tj ET 0 g\n")
}

/// Natural width of `text` at `size`, for centring decorations.
fn text_width(font: &FontInfo, size: f32, text: &str) -> f32 {
    let units: i32 = text.chars().map(|c| font.advance(font.gid(c))).sum();
    units as f32 / 1000.0 * size
}

/// Renders one page's content stream. `page_abs` is the 0-based
/// absolute page (for the footer), `chapter` the owning chapter.
fn page_content(
    font: &FontInfo,
    used: &mut BTreeMap<u16, char>,
    grid: &Grid,
    chapter: &ChapterInput,
    chapter_title: &str,
    page_local: usize,
    page_abs: usize,
    page_total: usize,
    hot: &[layout::Hot],
) -> Vec<u8> {
    let page = &chapter.pages[page_local];
    let mut s = String::new();

    // Running head: chapter title, top margin, small and gray.
    let head_size = 11.0;
    s.push_str(&show_text_at(
        font,
        used,
        head_size,
        grid.margin_x_pt,
        layout::PAGE_H_PT - 40.0,
        0.45,
        chapter_title,
    ));

    // Footer: "page / total", centred in the bottom margin.
    let footer = format!("{} / {}", page_abs + 1, page_total);
    let fw = text_width(font, 10.0, &footer);
    s.push_str(&show_text_at(
        font,
        used,
        10.0,
        (layout::PAGE_W_PT - fw) / 2.0,
        22.0,
        0.55,
        &footer,
    ));

    // Hot underlines, drawn *before* the text so ink sits on top.
    for h in hot {
        let (width, dash, gray) = underline_style(h.count);
        for seg in layout::underline_segments(&chapter.pages, h.off, h.len) {
            if seg.page != page_local {
                continue;
            }
            let y = layout::PAGE_H_PT - grid.baseline_pt(seg.row) - 3.0;
            let x0 = grid.col_x_pt(seg.col_start);
            let x1 = grid.col_x_pt(seg.col_end);
            let _ = writeln!(
                s,
                "q {gray:.2} G {width:.2} w {dash} {x0:.2} {y:.2} m {x1:.2} {y:.2} l S Q"
            );
        }
    }

    // Body text on the grid.
    s.push_str("BT /F1 ");
    let _ = write!(s, "{:.1}", grid.font_pt);
    s.push_str(" Tf\n");
    for (row, line) in page.lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let y = layout::PAGE_H_PT - grid.baseline_pt(row);
        let _ = writeln!(s, "1 0 0 1 {:.2} {y:.2} Tm {}", grid.margin_x_pt, line_tj(font, line, used));
    }
    s.push_str("ET\n");

    s.into_bytes()
}

/// Minimal PDF object store: bodies indexed by id, serialized in order
/// with a correct xref at the end.
struct Pdf {
    objs: Vec<Option<Vec<u8>>>,
}

impl Pdf {
    fn new() -> Self {
        Pdf { objs: Vec::new() }
    }
    fn alloc(&mut self) -> usize {
        self.objs.push(None);
        self.objs.len() // ids are 1-based
    }
    fn set(&mut self, id: usize, body: Vec<u8>) {
        self.objs[id - 1] = Some(body);
    }
    fn stream(dict_extra: &str, data: Vec<u8>) -> Vec<u8> {
        let mut b = format!("<< /Length {} /Filter /FlateDecode{} >>\nstream\n", data.len(), dict_extra).into_bytes();
        // Callers pass already-flated data; /Length1 etc. go in dict_extra.
        b.extend_from_slice(&data);
        b.extend_from_slice(b"\nendstream");
        b
    }
    fn finish(self, root: usize, info: usize) -> Vec<u8> {
        let mut out = b"%PDF-1.6\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::with_capacity(self.objs.len());
        for (i, body) in self.objs.iter().enumerate() {
            offsets.push(out.len());
            let body = body.as_deref().unwrap_or(b"null");
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", self.objs.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {root} 0 R /Info {info} 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
                self.objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}

/// Generates the whole-book PDF matching `layout` (which must have been
/// built from these same `chapters` — same order, same pages).
pub fn generate(book: &BookLayout, chapters: &[ChapterInput]) -> Result<Vec<u8>, String> {
    generate_with_font(book, chapters, FONT)
}

pub fn generate_with_font(
    book: &BookLayout,
    chapters: &[ChapterInput],
    font_data: &[u8],
) -> Result<Vec<u8>, String> {
    if book.chapters.len() != chapters.len() {
        return Err("layout/chapter count mismatch".into());
    }
    let font = FontInfo::parse(font_data)?;
    let grid = &book.grid;

    let mut pdf = Pdf::new();
    let catalog_id = pdf.alloc();
    let pages_id = pdf.alloc();
    let info_id = pdf.alloc();
    let type0_id = pdf.alloc();
    let cidfont_id = pdf.alloc();
    let descriptor_id = pdf.alloc();
    let fontfile_id = pdf.alloc();
    let tounicode_id = pdf.alloc();
    let outlines_id = pdf.alloc();
    let outline_ids: Vec<usize> = chapters.iter().map(|_| pdf.alloc()).collect();
    // Page and content ids, chapter by chapter.
    let mut page_ids = Vec::new();
    for c in chapters {
        for _ in &c.pages {
            let p = pdf.alloc();
            let s = pdf.alloc();
            page_ids.push((p, s));
        }
    }
    let total_pages = page_ids.len();
    if total_pages != book.page_count {
        return Err("layout/page count mismatch".into());
    }

    // ---- Pages + content ----
    let mut used: BTreeMap<u16, char> = BTreeMap::new();
    let mut page_cursor = 0usize;
    for (ci, chapter) in chapters.iter().enumerate() {
        // Draw straight from the frozen layout's hot list: it already
        // carries range/off/len/count in a deterministic order, which
        // is what byte-identical output for the same inputs depends on.
        let clay = &book.chapters[ci];

        for (pi, _) in chapter.pages.iter().enumerate() {
            let (page_id, content_id) = page_ids[page_cursor];
            let content = page_content(
                &font,
                &mut used,
                grid,
                chapter,
                &chapter.title,
                pi,
                page_cursor,
                total_pages,
                &clay.hot,
            );
            pdf.set(content_id, Pdf::stream("", flate(&content)));
            pdf.set(
                page_id,
                format!(
                    "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {} {}] \
                     /Resources << /Font << /F1 {type0_id} 0 R >> >> /Contents {content_id} 0 R >>",
                    layout::PAGE_W_PT,
                    layout::PAGE_H_PT
                )
                .into_bytes(),
            );
            page_cursor += 1;
        }
    }

    // ---- Page tree ----
    let kids: Vec<String> = page_ids.iter().map(|(p, _)| format!("{p} 0 R")).collect();
    pdf.set(
        pages_id,
        format!("<< /Type /Pages /Kids [{}] /Count {total_pages} >>", kids.join(" ")).into_bytes(),
    );

    // ---- Font ----
    used.entry(0).or_insert('\u{0}'); // .notdef, so W always has an entry
    let w_entries: Vec<String> = used.keys().map(|gid| format!("{gid} [{}]", font.advance(*gid))).collect();
    pdf.set(
        type0_id,
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /NotoSansCJKsc /Encoding /Identity-H \
             /DescendantFonts [{cidfont_id} 0 R] /ToUnicode {tounicode_id} 0 R >>"
        )
        .into_bytes(),
    );
    pdf.set(
        cidfont_id,
        format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /NotoSansCJKsc \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /FontDescriptor {descriptor_id} 0 R /DW 1000 /W [{}] /CIDToGIDMap /Identity >>",
            w_entries.join(" ")
        )
        .into_bytes(),
    );
    let bbox = font.face.global_bounding_box();
    let sc = font.scale;
    pdf.set(
        descriptor_id,
        format!(
            "<< /Type /FontDescriptor /FontName /NotoSansCJKsc /Flags 4 \
             /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} \
             /CapHeight {} /StemV 80 /FontFile2 {fontfile_id} 0 R >>",
            (bbox.x_min as f32 * sc) as i32,
            (bbox.y_min as f32 * sc) as i32,
            (bbox.x_max as f32 * sc) as i32,
            (bbox.y_max as f32 * sc) as i32,
            (font.face.ascender() as f32 * sc) as i32,
            (font.face.descender() as f32 * sc) as i32,
            (font.face.ascender() as f32 * sc * 0.7) as i32,
        )
        .into_bytes(),
    );
    pdf.set(
        fontfile_id,
        Pdf::stream(&format!(" /Length1 {}", font_data.len()), flate(font_data)),
    );

    // ToUnicode CMap so text extraction / native selection works.
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, char)> = used.iter().filter(|(_, c)| **c != '\u{0}').map(|(g, c)| (*g, *c)).collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        for (gid, c) in chunk {
            let mut u = String::new();
            for cu in c.encode_utf16([0u16; 2].as_mut()) {
                let _ = write!(u, "{cu:04X}");
            }
            let _ = writeln!(cmap, "<{gid:04X}> <{u}>");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    pdf.set(tounicode_id, Pdf::stream("", flate(cmap.as_bytes())));

    // ---- Outlines: one entry per chapter ----
    for (ci, oid) in outline_ids.iter().enumerate() {
        let clay = &book.chapters[ci];
        let (dest_page, _) = page_ids[clay.page_start];
        let prev = (ci > 0).then(|| format!(" /Prev {} 0 R", outline_ids[ci - 1]));
        let next = (ci + 1 < outline_ids.len()).then(|| format!(" /Next {} 0 R", outline_ids[ci + 1]));
        pdf.set(
            *oid,
            format!(
                "<< /Title {} /Parent {outlines_id} 0 R{}{} /Dest [{dest_page} 0 R /XYZ null null null] >>",
                utf16_hex(&clay.title),
                prev.unwrap_or_default(),
                next.unwrap_or_default(),
            )
            .into_bytes(),
        );
    }
    if let (Some(first), Some(last)) = (outline_ids.first(), outline_ids.last()) {
        pdf.set(
            outlines_id,
            format!(
                "<< /Type /Outlines /First {first} 0 R /Last {last} 0 R /Count {} >>",
                outline_ids.len()
            )
            .into_bytes(),
        );
    } else {
        pdf.set(outlines_id, b"<< /Type /Outlines /Count 0 >>".to_vec());
    }

    // ---- Catalog + Info ----
    pdf.set(
        catalog_id,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R /Outlines {outlines_id} 0 R >>").into_bytes(),
    );
    pdf.set(
        info_id,
        format!(
            "<< /Title {} /Author {} /Producer (rm-weread) >>",
            utf16_hex(&book.title),
            utf16_hex(&book.author)
        )
        .into_bytes(),
    );

    Ok(pdf.finish(catalog_id, info_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Grid, HotInput, build};
    use crate::paginate::paginate;

    fn small_book() -> (BookLayout, Vec<ChapterInput>) {
        let grid = Grid { cols: 10, lines_per_page: 4, ..Grid::default() };
        let text1 = "第一章的正文，有一句被很多人划过的话。";
        let text2 = "第二章 short mixed 内容。";
        let chapters = vec![
            ChapterInput {
                chapter_uid: 11,
                title: "第一章".into(),
                text: text1.into(),
                pages: paginate(text1, grid.cols, grid.lines_per_page),
                hot: vec![HotInput { range: "10-20".into(), off: 8, len: 6, count: 1234 }],
            },
            ChapterInput {
                chapter_uid: 12,
                title: "第二章".into(),
                text: text2.into(),
                pages: paginate(text2, grid.cols, grid.lines_per_page),
                hot: vec![],
            },
        ];
        let layout = build("book1", "测试书", "作者", &chapters, grid);
        (layout, chapters)
    }

    #[test]
    fn generates_a_structurally_sound_pdf() {
        let (layout, chapters) = small_book();
        let pdf = generate(&layout, &chapters).unwrap();
        let head = &pdf[..8.min(pdf.len())];
        assert_eq!(head, b"%PDF-1.6");
        let tail = String::from_utf8_lossy(&pdf[pdf.len() - 200..]);
        assert!(tail.contains("%%EOF"));
        assert!(tail.contains("startxref"));
        // Every object body was set (no `null` placeholders survived).
        assert!(!String::from_utf8_lossy(&pdf).contains("obj\nnull\nendobj"));
    }

    #[test]
    fn xref_offsets_point_at_objects() {
        // Work on raw bytes: the PDF contains binary streams, so
        // lossy-UTF8 string indices would drift from byte offsets.
        let (layout, chapters) = small_book();
        let pdf = generate(&layout, &chapters).unwrap();
        let tail = String::from_utf8_lossy(&pdf[pdf.len() - 100..]);
        let xref_at: usize = tail
            .split("startxref\n")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(&pdf[xref_at..xref_at + 4], b"xref");
        // The first real entry (line 3 of the ASCII xref section)
        // points at "1 0 obj".
        let section = String::from_utf8_lossy(&pdf[xref_at..xref_at + 200]);
        let off: usize = section.lines().nth(3).unwrap()[..10].parse().unwrap();
        assert_eq!(&pdf[off..off + 7], b"1 0 obj");
    }

    #[test]
    fn output_is_deterministic() {
        let (layout, chapters) = small_book();
        let a = generate(&layout, &chapters).unwrap();
        let b = generate(&layout, &chapters).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn decoration_refresh_only_changes_streams_not_geometry() {
        // Same text, different hot set: page count and object layout
        // stay identical — the invariant a swap-under-the-document
        // refresh depends on.
        let (layout_a, chapters_a) = small_book();
        let mut chapters_b: Vec<ChapterInput> = chapters_a
            .iter()
            .map(|c| ChapterInput {
                chapter_uid: c.chapter_uid,
                title: c.title.clone(),
                text: c.text.clone(),
                pages: c.pages.clone(),
                hot: vec![],
            })
            .collect();
        chapters_b[1].hot.push(HotInput { range: "5-6".into(), off: 0, len: 3, count: 5 });
        let grid = layout_a.grid;
        let layout_b = build("book1", "测试书", "作者", &chapters_b, grid);
        assert_eq!(layout_a.content_sha256, layout_b.content_sha256);
        assert_eq!(layout_a.page_count, layout_b.page_count);
        let a = generate(&layout_a, &chapters_a).unwrap();
        let b = generate(&layout_b, &chapters_b).unwrap();
        assert_ne!(a, b); // different decorations...
        assert_eq!(a.len().min(b.len()) > 1000, true);
    }

    #[test]
    fn refuses_mismatched_inputs() {
        let (layout, mut chapters) = small_book();
        chapters.pop();
        assert!(generate(&layout, &chapters).is_err());
    }

    #[test]
    fn embedded_font_covers_the_needed_glyphs() {
        let font = FontInfo::parse(FONT).unwrap();
        for c in ['中', '，', '。', 'A', '1', '①', '⑳', '/'] {
            assert_ne!(font.gid(c), 0, "missing glyph for {c:?}");
        }
        // CJK is exactly two columns wide at the grid's math.
        assert_eq!(font.advance(font.gid('中')), 1000);
    }
}
