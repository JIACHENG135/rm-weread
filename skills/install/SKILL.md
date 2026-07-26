---
name: rm-weread-install
description: Install or update rm-weread on a reMarkable 2 / Paper Pro over SSH — XOVI extensions, the qmldiff QML patch, the daemon, and the WeRead login session. Use when asked to install, deploy, set up, or update rm-weread on a device, or when the review popup / four-finger generate stops working after a firmware update.
---

# Installing rm-weread on a device

This installs a QML patch **into xochitl's own process**. A bad patch
has previously put a device into a reboot loop (recorded in `rm-agent`'s
deploy notes). Step 7's foreground validation is therefore not optional
— never skip from installing a `.qmd` straight to the persistent
`xovi/start`.

Everything below is done over SSH as `root`. Nothing here needs the
reMarkable cloud.

## 0. Know the two devices apart

| | reMarkable 2 | Paper Pro |
|---|---|---|
| `uname -m` | `armv7l` | `aarch64` |
| hostname | `reMarkable` | `imx8mm-ferrari` |
| Rust target | `armv7-unknown-linux-musleabihf` | `aarch64-unknown-linux-musl` |
| screen | 1404×1872 | 1620×2160 |

Both are 3:4, which is why the generated PDF (also 3:4) and the
normalized coordinates in `layout.json` work unchanged on either.

Get the arch first; every later step branches on it:

```sh
ssh root@<host> 'uname -m; hostname; cat /etc/version'
```

**If SSH times out**, do not assume the device is off. These get their
address by DHCP and it moves. Scan the subnet for an open port 22 and
identify the host before concluding anything:

```sh
for i in $(seq 2 254); do (nc -z -G1 192.168.1.$i 22 2>/dev/null && echo "$i") & done; wait
```

## 1. Build

From the repo root, on the workstation (not the device):

```sh
cargo test                                                    # must be green first
cargo build --release --target aarch64-unknown-linux-musl --bin weread_daemon
```

Substitute the armv7 target for an rM2. Both linkers are configured in
`.cargo/config.toml` and must be on `PATH`.

## 2. XOVI framework

Skip if `/home/root/xovi/xovi.so` already exists.

Download the release asset matching the device's arch from
[`asivery/rm-xovi-extensions`](https://github.com/asivery/rm-xovi-extensions/releases)
(`xovi-arm32` for rM2, the aarch64 asset for Paper Pro), extract it, and
`scp -r` the tree to `/home/root/xovi`.

`scp` silently drops two symlinks in the tarball. Recreate them on the
device or services will not pick up extensions:

```sh
ln -sf /home/root/xovi/extensions.d /home/root/xovi/services/xochitl.service/extensions.d
ln -sf /home/root/xovi/exthome      /home/root/xovi/services/xochitl.service/exthome
```

## 3. Activate the two extensions this project needs

`qt-resource-rebuilder` applies the QML patch; `qt-command-executor`
gives the patch its only way to talk to the daemon (it can run
`/bin/touch`, nothing else — which is why every IPC parameter is encoded
into a *filename*, see `docs/design.md`).

```sh
for ext in qt-resource-rebuilder.so qt-command-executor.so; do
    [ -f "/home/root/xovi/inactive-extensions/$ext" ] &&
        mv "/home/root/xovi/inactive-extensions/$ext" /home/root/xovi/extensions.d/
done
ls /home/root/xovi/extensions.d/
```

## 4. Hashtable for this exact firmware

**Hashtables are not portable across firmware versions.** A device on a
version you have not seen before needs its own, built on-device:

```sh
ssh -t root@<host> '/home/root/xovi/rebuild_hashtable'
```

This runs xochitl once and **needs a human to unlock the tablet with its
passcode** when prompted — it cannot be automated. It also stops
xochitl, so `systemctl start xochitl` afterwards.

Cache the result (`/home/root/xovi/exthome/qt-resource-rebuilder/hashtab`)
per firmware version on the workstation; later installs to the same
firmware just copy it back.

## 5. Install the QML patch

```sh
scp xovi/weread.qmd root@<host>:/home/root/xovi/exthome/qt-resource-rebuilder/
```

On reMarkable 3.27.3.0 the shipped QML is **not** identifier-obfuscated
and the plaintext `.qmd` applies as-is (verified on a Paper Pro). If a
future firmware obfuscates it, the identifiers stop resolving; hash the
file against that device's hashtab first:

```sh
qmldiff hash-diffs <hashtab> weread.qmd     # github.com/asivery/qmldiff
```

Note `weread.qmd`'s own warning: the `.qmd` grammar rejects top-level
`//` comments, so every comment must live inside the `INSERT` block. It
fails silently.

## 6. Daemon, font, and the systemd unit

```sh
scp target/<triple>/release/weread_daemon root@<host>:/home/root/weread_daemon
scp xovi/weread-daemon.service root@<host>:/etc/systemd/system/
ssh root@<host> 'chmod +x /home/root/weread_daemon; mkdir -p /home/root/xovi/exthome/weread; systemctl daemon-reload'
```

The popup renders Chinese, so a CJK font must exist at the path
`weread.qmd`'s `FontLoader` points at:
`/home/root/xovi/exthome/weread/NotoSansSC.ttf`. Any Noto Sans SC build
works. This is **not** the same font as `assets/NotoSansSC-Regular.ttf`,
which is embedded into the generated PDF and ships in the repo.

Run the daemon under systemd, never from an SSH pipe: its stdout on a
tty that later disappears fills the pipe buffer and a blocked `println!`
deadlocks the whole daemon. This was hit for real; the unit exists so
journald owns the output.

## 7. Validate under `xovi/debug` before making it persistent

Non-negotiable. `xovi/debug` runs xochitl in the foreground where a
fatal QML error is visible and recoverable.

Detach it on the device and log to a file — do not hold it open on the
SSH pipe (same deadlock as above):

```sh
ssh root@<host> '
  systemctl stop xochitl; sleep 1
  setsid sh -c "nohup /home/root/xovi/debug > /home/root/xovi_debug.log 2>&1 &" </dev/null >/dev/null 2>&1
'
sleep 25
ssh root@<host> '
  echo "pid: $(pidof xochitl)"
  grep -i qmldiff /home/root/xovi_debug.log
  grep -icE "Unable to assign|is not a type|Cannot assign|SyntaxError|Expected token" /home/root/xovi_debug.log
'
```

Expect to see `Loading file weread.qmd`, `Processing file
/qml/device/view/documentview/SceneViewGestures.qml`, a live pid, and an
error count of `0`.

`Experimental.qml:76: ReferenceError: Values is not defined` is stock
xochitl noise on this firmware, not caused by this patch.

**If there are errors**: `/home/root/xovi/stock` returns the device to
unpatched xochitl. Fix the `.qmd` and repeat — do not continue.

## 8. Go persistent

```sh
ssh root@<host> 'pidof xochitl | xargs -r kill; sleep 2; /home/root/xovi/start'
ssh root@<host> 'systemctl enable --now weread-daemon'
```

## 9. Log in

Login runs on a **workstation**, not the device — it prints a URL to
turn into a QR code:

```sh
cargo run --bin weread_login          # writes ./session.json
scp session.json root@<host>:/home/root/.local/share/rm-weread/session.json
ssh root@<host> 'systemctl restart weread-daemon'
```

The account must first enable the WeRead Skill in the phone app
(我 → 设置 → 微信读书 Skill → 获取 API Key). Without it there is no
`apikey` and the shelf call fails — an account-side switch, not a bug.

`session.json` holds live cookies and that API key. It is gitignored and
must never be committed.

## 10. Smoke test without touching the device

```sh
ssh root@<host> 'touch /home/root/xovi/exthome/weread/generate'
ssh root@<host> 'journalctl -u weread-daemon -f'
```

A whole book takes minutes. Poll for completion by watching the
**sequence number** on line 1 of `gen.txt` — not the status word on line
2, which still holds the *previous* run's `done` and will make a naive
loop exit immediately:

```sh
ssh root@<host> 'cat /home/root/xovi/exthome/weread/gen.txt'
```

Then confirm the artifacts:

```sh
ssh root@<host> '
  ls -l /home/root/xovi/exthome/weread/layout.json
  ls -l /home/root/.local/share/remarkable/xochitl/*.pdf | tail -5
'
```

If the new document does not appear in the library, restart xochitl once.

## What is installed where

| Path | What |
|---|---|
| `/home/root/weread_daemon` | the binary |
| `/etc/systemd/system/weread-daemon.service` | unit |
| `/home/root/xovi/exthome/qt-resource-rebuilder/weread.qmd` | QML patch |
| `/home/root/xovi/exthome/weread/` | QML-facing IPC: `layout.json`, `gen.txt`, `reviews.txt`, `NotoSansSC.ttf` |
| `/home/root/.local/share/rm-weread/` | `session.json`, chapter cache, per-book layouts, `docs.json` registry |
| `/home/root/.local/share/remarkable/xochitl/` | the generated PDF documents |

## Updating an existing install

Only the changed pieces:

- **Daemon only**: `systemctl stop weread-daemon`, replace the binary,
  `systemctl start`. No xochitl restart.
- **QML only**: replace the `.qmd`, then redo step 7 **and** step 8. A
  `.qmd` change needs an xochitl restart to take effect, and it gets the
  same foreground validation as a fresh install.
- **Regenerate a book**: `touch .../weread/generate`.

Regenerating is safe for existing ink as long as the chapter text and
grid constants are unchanged: `content_sha256` stays equal and
`xochitl_doc.rs` does an in-place decoration refresh. If that hash *does*
change and the document already has `.rm` ink, the pipeline refuses to
overwrite and creates a "(更新版)" sibling instead. Never work around
that.

## Troubleshooting

- **Popup never fires, generation works** — the tap gating is
  `view.document.id == layout.json`'s `doc_uuid`. It only fires inside
  the generated document. Check that `doc_uuid` is non-empty.
- **HTTP 499 on `/book/underlines`** — WeRead throttling after repeated
  full regenerations. Chapters are skipped with a warning rather than
  failing the run; wait rather than retrying in a loop.
- **`upgrade_info` in a shelf response** — a "new version available"
  side note, *not* an error. Do not reject the response over it.
- **After a firmware update** — the hashtab is invalid (step 4) and any
  QML property names the patch relies on may have moved. There is no QML
  on disk to read; it lives in xochitl's qrc. Re-derive names by walking
  the parent chain from inside the patch and logging property names
  under `xovi/debug`.
