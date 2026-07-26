# rm-weread

A 微信读书 (WeRead) client for reMarkable tablets (reMarkable 2 and Paper
Pro): one Rust binary plus a small QML patch. No companion app, no
account of our own, no backend to maintain — WeRead's own servers are
the only backend, and everything here runs on the device.

*[中文](README.md) · the full record of the design and its trade-offs is
in [docs/design.md](docs/design.md) (Chinese)*

## What it does

Pick a book from your WeRead shelf. The daemon fetches and decodes the
chapters, lays them out, and delivers the resulting PDF **into xochitl's
own library**. You read it in the tablet's **native PDF reader**.

That is the whole design. Reading natively means native ink latency,
strokes saved into the correct `.rm` file, and page turns, thumbnails
and the table of contents for free. An earlier version rendered the text
itself in QML; it worked on real hardware and was thrown away anyway,
because ink could never work inside it — xochitl's writing engine draws
straight to the framebuffer, bypassing Qt compositing, and wrote strokes
into whichever document was open underneath.

Other readers' **popular highlights** are drawn over the page as you
reach it, and **touching an underline** shows what they wrote about that
passage.

```
Four-finger tap in any open document, or open the "＋ 书架" card in the
微信读书 folder
  → shelf browser (cover / title / author / ✓ if already here)
  → pick one → progress bar → a "refresh library" button when it lands
```

## Status

Working end to end on a **reMarkable Paper Pro** and a **reMarkable 2**
(firmware 3.27.3.0), sharing one WeRead account.

Known limitations, all deliberate:

- **Read-only.** Highlights and notes are not written back to WeRead,
  and reading progress is not reported. Your ink lives in the reMarkable
  document, which is what a reMarkable annotation *is*.
- **Layout is frozen.** Ink is anchored to page geometry, so a new PDF
  may only be swapped into an existing document while the hash of the
  text and the layout constants is unchanged. If it changes and the
  document already has ink, the pipeline **refuses to overwrite** and
  produces a "(更新版)" sibling instead.
- **Chapter images are not rendered yet.** Fix that *before* first
  generating a book you intend to annotate — adding them later moves the
  geometry and invalidates that book's ink.
- **Live underlines are not written into the PDF.** An exported or
  synced copy is text only.

## Installing

Download the binary for your device from
[Releases](https://github.com/JIACHENG135/rm-weread/releases)
(`weread_daemon-aarch64` for Paper Pro, `weread_daemon-armv7` for
reMarkable 2) and follow
[`skills/install/SKILL.md`](skills/install/SKILL.md) — written to be
executed by an agent, equally readable by a person.

Two things in there are not optional. Neither is guessable, and each
cost hours:

1. **`xovi/debug` cannot tell you whether this works.** It sets
   `LD_PRELOAD` but *not* `XOVI_ROOT`, so the native extensions never
   load, `CommandExecutor` is an unknown type, and the entire injected
   block fails to instantiate — **silently**, while qmldiff still
   reports "Processing file …" and the log shows zero errors. Validate
   under `systemctl start xochitl`.
2. **On the Paper Pro, `/etc` is an overlay on tmpfs.** Anything written
   to `/etc/systemd/system` is lost on reboot, including the symlink
   `systemctl enable` creates. Put units on the read-only root. The rM2
   is not like this — check `mount | grep 'on /etc'` first.

## Request discipline

Generating a book makes **zero** underline requests. An earlier version
fetched every chapter's highlights up front — 288 requests in a few
minutes for a 288-chapter book — and WeRead answered with HTTP 499s and
then blocked the whole gateway with a 403 (the same key worked from
`wget` on the same device at the same moment, so the block was on the
client, not the account).

Highlights are now fetched **one chapter at a time, as you reach it**.
Reviews were always fetched on tap.

## Layout of the code

| Module | Role |
|---|---|
| `login.rs` / `session.rs` / `cookie.rs` | QR login, session persistence, cookie renewal |
| `skill_gateway.rs` / `shelf.rs` | WeRead Skill gateway, shelf |
| `weread_sign.rs` | request signing (`_e` / `sign`) |
| `content.rs` | chapter decoding — the one WeRead-proprietary algorithm |
| `reader.rs` / `xhtml.rs` | chapter fetch; XHTML → text with an offset map |
| `underlines.rs` | highlights and reviews; range → text offset |
| `metrics.rs` | glyph widths, the single source pagination, layout and drawing all share |
| `paginate.rs` / `layout.rs` | pagination; frozen geometry (`layout.json`) |
| `pdfgen.rs` | hand-written, deterministic PDF with an embedded CJK font |
| `xochitl_doc.rs` | delivery into the library + the freeze rules |
| `pipeline.rs` | end to end |
| `xovi/weread.qmd` | the QML patch: shelf browser, underline overlay, review popup |

`bin/weread_daemon.rs` runs on the device. `bin/weread_login.rs`,
`weread_chapter.rs`, `weread_page.rs` and `weread_pdf.rs` are
workstation CLIs for driving parts of the pipeline against a real
account.

## Building and testing

Cross-compiled for reMarkable (linkers configured in
[`.cargo/config.toml`](.cargo/config.toml)):

```sh
cargo build --release --target armv7-unknown-linux-musleabihf   # reMarkable 2
cargo build --release --target aarch64-unknown-linux-musl       # Paper Pro
```

Tests need an explicit host target, because the repo cross-compiles by
default:

```sh
cargo test --target aarch64-apple-darwin
```

Decoding, signing, pagination, layout and PDF generation are all offline
and unit-tested — including against real captured API responses, and
against ground truth produced by running the original Lua implementation
under a Lua interpreter.

## Credits

The endpoint contracts and the chapter-decoding algorithm are ported
from [finlater/weread.koplugin](https://github.com/finlater/weread.koplugin)
and its API reference, which is where the reverse engineering actually
happened. [REweread](https://github.com/nasonliu/REweread) is the prior
reMarkable client; this project takes a different route, skipping the
KOReader/LuaJIT runtime entirely and porting only the algorithms.

The QML patching mechanism is [asivery](https://github.com/asivery)'s
XOVI, `qt-resource-rebuilder` and `qmldiff`.

## Caveats

WeRead's API is undocumented and will change; when it does, fixing it is
this project's problem alone. Generating a PDF leaves a complete local
copy of a purchased book on the device — it stays on your own device,
and is never uploaded or shared.

**Personal, non-commercial use only.** The API is not authorized by
Tencent, and this project is not affiliated with WeRead or reMarkable.
