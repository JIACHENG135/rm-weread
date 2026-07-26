//! Generates a small sample PDF offline (no account needed) so the
//! output can be eyeballed in a real viewer and sanity-checked with
//! external PDF tools. Run:
//!
//!   cargo run --example sample_pdf --target <host-triple>
//!
//! Writes target/sample.pdf.

use rm_weread::layout::{Grid, HotInput, build};
use rm_weread::paginate::paginate;
use rm_weread::pdfgen;

fn main() {
    let grid = Grid::default();
    let body1 = "白色的球状闪电在雨夜里安静地悬浮着，像一个来自另一个世界的访客。\
                 它不发出任何声音，也没有灼热的气息，只是缓缓地飘过桌面，掠过书架，\
                 然后在墙壁前停住了。那一刻我意识到，物理学在这里失效了——或者说，\
                 我们所知道的那部分物理学失效了。Quantum mechanics tells a different story.\n\
                 第二段：许多年以后，我仍然会想起那个夜晚的每一个细节。"
        .repeat(6);
    let body2 = "第二章的内容要短一些，但同样有一句被很多人划过的话，\
                 以及一句几乎没有人注意到的话。"
        .to_string();

    let chapters = vec![
        rm_weread::layout::ChapterInput {
            chapter_uid: 1,
            title: "第一章 球状闪电".into(),
            pages: paginate(&body1, grid.text_em, grid.lines_per_page),
            hot: vec![
                HotInput { range: "100-140".into(), off: 10, len: 22, count: 2311 },
                HotInput { range: "300-330".into(), off: 60, len: 18, count: 154 },
                HotInput { range: "500-510".into(), off: 130, len: 12, count: 23 },
            ],
            text: body1,
        },
        rm_weread::layout::ChapterInput {
            chapter_uid: 2,
            title: "第二章".into(),
            pages: paginate(&body2, grid.text_em, grid.lines_per_page),
            hot: vec![HotInput { range: "40-60".into(), off: 8, len: 10, count: 999 }],
            text: body2,
        },
    ];

    let layout = build("sample", "球状闪电（样例）", "刘慈欣", &chapters, grid, false);
    let pdf = pdfgen::generate(&layout, &chapters, None).expect("generate");
    std::fs::write("target/sample.pdf", &pdf).expect("write");
    println!(
        "target/sample.pdf: {} bytes, {} pages, {} underlines, {} tap targets",
        pdf.len(),
        layout.page_count,
        layout.hot_count(),
        layout.taps.len()
    );
    println!("layout.json: {} bytes", serde_json::to_string(&layout).unwrap().len());
}
