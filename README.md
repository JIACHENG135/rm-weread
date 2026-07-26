# rm-weread

A native 微信读书 (WeRead) client for reMarkable tablets (reMarkable 2 and
Paper Pro), built as a single Rust binary + a small QML patch — no
companion app, no cloud account of our own, no backend to maintain.
WeRead's own servers are the only backend; this project is a thin client
on top of them, running entirely on the device.

Design and rationale, including the decisions that were reversed and
why: [docs/design.md](docs/design.md).

## What it does

The daemon pulls a book from your WeRead shelf, decodes the chapter
text, lays it out on a fixed character grid, and generates a **PDF that
it delivers into xochitl's own library**. You read it in the tablet's
native PDF reader.

That last part is the whole design. Reading in the native reader means
native ink latency, strokes stored in the correct `.rm` file, and page
turns, thumbnails, and the table of contents all for free. An earlier
version rendered the text itself in a fullscreen QML reader; it worked
on real hardware and was still thrown away, because ink could never work
inside it — xochitl's writing engine draws straight to the framebuffer,
bypassing Qt compositing, and wrote strokes into whichever document was
open *underneath*. See §"已废弃：全屏 QML 阅读器" in the design doc.

Popular highlights from other readers are burned into the PDF as dashed
gray underlines, weighted by heat. **Touch the underlined words** and a
QML popup fetches that passage's reader comments on demand.

The QML patch is deliberately tiny — a popup and a four-finger gesture,
nothing more.

## Status

Working end to end, validated on a real Paper Pro (firmware 3.27.3.0).

A full run on a 40-chapter book produces a 300-page, ~4.9 MB PDF with
840 hot underlines and 2134 tap targets, and the popup returns real
comment threads.

Known limitations, all deliberate:

- **Read-only.** Highlights and notes are not written back to your
  WeRead account. Your ink lives in the reMarkable document, which is
  what a reMarkable annotation *is*.
- **Reading progress is not reported back** to WeRead. A consequence of
  being read-only, not a bug.
- **Layout is frozen** once generated. Ink is anchored to page geometry,
  so the pipeline may only swap a PDF under an existing document when
  the text and grid constants hash identically. If they change and the
  document already has ink, it refuses to overwrite and produces a
  "(更新版)" sibling instead.
- **Images in chapters are not rendered yet.** This must be fixed
  *before* first generating a book you intend to annotate — adding them
  later changes the geometry and invalidates that book's existing ink.
- One book at a time (a single "current book" slot).

## Installing

See [`skills/install/SKILL.md`](skills/install/SKILL.md) — a Claude Code
skill, but equally readable as the manual procedure. It covers the XOVI
extensions, the per-firmware qmldiff hashtable, the QML patch, the
daemon and its systemd unit, and login.

Two things in there are not optional:

1. **Validate the QML patch under `xovi/debug` before making it
   persistent.** This patch runs inside xochitl's process. A wrong
   property name has previously put a device into a reboot loop.
2. **Run the daemon under systemd, not from an SSH pipe.** Its stdout on
   a tty that later disappears fills the pipe buffer, and a blocked
   `println!` deadlocks the daemon.

## Building

Cross-compiled for reMarkable targets (linkers configured in
[`.cargo/config.toml`](.cargo/config.toml)):

```sh
cargo build --release --target armv7-unknown-linux-musleabihf   # reMarkable 2
cargo build --release --target aarch64-unknown-linux-musl       # Paper Pro
```

`cargo test` runs the whole suite on the host. It needs an explicit host
target, because the repo defaults to cross-compiling:

```sh
cargo test --target aarch64-apple-darwin
```

The decode, signing, pagination, layout, and PDF logic are all offline
and unit-tested — including against real captured API responses, and
against ground truth produced by running the original Lua implementation
under a Lua interpreter.

## Layout of the code

| Module | Role |
|---|---|
| `login.rs`, `session.rs`, `cookie.rs` | QR login, session persistence, cookie renewal |
| `skill_gateway.rs`, `shelf.rs` | WeRead Skill API gateway, shelf |
| `weread_sign.rs` | request signing (`_e` / `sign`) |
| `content.rs` | chapter decoding — the one WeRead-proprietary algorithm |
| `reader.rs`, `xhtml.rs` | chapter fetch; XHTML → text with an offset map |
| `underlines.rs` | hot underlines and reviews; range → text offset |
| `paginate.rs`, `layout.rs` | character-grid pagination; frozen geometry (`layout.json`) |
| `pdfgen.rs` | hand-written, deterministic PDF with an embedded CJK font |
| `xochitl_doc.rs` | delivery into xochitl's library + the freeze rules |
| `pipeline.rs` | end to end, plus threshold-gated refresh |
| `xovi/weread.qmd` | the QML patch: tap-an-underline popup, four-finger generate |

`bin/weread_daemon.rs` is what runs on the device. `bin/weread_login.rs`,
`bin/weread_chapter.rs`, `bin/weread_page.rs`, and `bin/weread_pdf.rs`
are workstation CLIs for driving parts of the pipeline against a real
account.

## Credits

The endpoint contracts and the chapter-decoding algorithm were ported
from [finlater/weread.koplugin](https://github.com/finlater/weread.koplugin)
(and its API reference), which is where that reverse-engineering work
actually happened. [REweread](https://github.com/nasonliu/REweread) is
the prior reMarkable client; this project takes a different route,
skipping the KOReader/LuaJIT runtime entirely and porting only the
algorithms.

The QML patching mechanism is [asivery](https://github.com/asivery)'s
XOVI, `qt-resource-rebuilder`, and `qmldiff`.

## Caveats

WeRead's API is undocumented and will change; when it does, fixing it is
this project's problem alone. Generating a PDF puts a complete local
copy of a purchased book on the device — it stays on your own device,
and is never uploaded or shared.

For personal, non-commercial use only. The API is not authorized by
Tencent, and this project is not affiliated with WeRead or reMarkable.
