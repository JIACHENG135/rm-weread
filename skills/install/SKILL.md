---
name: rm-weread-install
description: Install, update or repair rm-weread on a reMarkable 2 / Paper Pro over SSH — XOVI extensions, the qmldiff QML patch, the daemon, login, and the persistence and font fixes this firmware needs. Use when asked to install, deploy, set up, update or fix rm-weread on a device, or when the shelf browser / review popup stops working.
---

# Installing rm-weread on a device

Everything here is done over SSH as `root`. Nothing needs the reMarkable
cloud, and nothing needs a Rust toolchain — the release ships built
binaries.

Two rules are worth reading before anything else, because both cost a
long debugging session to learn and neither is guessable:

1. **`xovi/debug` cannot tell you whether this works.** See step 7.
2. **`/etc` does not survive a reboot on this firmware.** See step 6.

## 0. Identify the device

| | reMarkable 2 | Paper Pro |
|---|---|---|
| `uname -m` | `armv7l` | `aarch64` |
| hostname | `reMarkable` | `imx8mm-ferrari` |
| release asset | `weread_daemon-armv7` | `weread_daemon-aarch64` |

```sh
ssh root@<host> 'uname -m; hostname; cat /etc/version'
```

**If SSH times out, do not assume the device is off.** These get their
address by DHCP and it moves. Scan for an open port 22 and identify what
answers before concluding anything:

```sh
for i in $(seq 2 254); do (nc -z -G1 192.168.1.$i 22 2>/dev/null && echo "$i") & done; wait
```

## 1. Get the binary

Download the asset matching the arch from the latest release:

```sh
gh release download --repo JIACHENG135/rm-weread --pattern 'weread_daemon-aarch64'
```

Building from source instead needs the musl cross-linkers named in
`.cargo/config.toml` on `PATH`; `cargo test` needs an explicit host
target because the repo cross-compiles by default
(`cargo test --target aarch64-apple-darwin`).

## 2. XOVI framework

Skip if `/home/root/xovi/xovi.so` exists. Otherwise take the release
asset for the device's arch from
[`asivery/rm-xovi-extensions`](https://github.com/asivery/rm-xovi-extensions/releases),
extract it, and `scp -r` the tree to `/home/root/xovi`.

`scp` silently drops two symlinks in the tarball. Recreate them or
services will not pick up extensions:

```sh
ln -sf /home/root/xovi/extensions.d /home/root/xovi/services/xochitl.service/extensions.d
ln -sf /home/root/xovi/exthome      /home/root/xovi/services/xochitl.service/exthome
```

## 3. Activate the two extensions

`qt-resource-rebuilder` applies the QML patch. `qt-command-executor`
gives the patch its only way to talk to the daemon — it can run
`/bin/touch` and nothing else, which is why every IPC parameter is
encoded into a *filename*.

```sh
for ext in qt-resource-rebuilder.so qt-command-executor.so; do
    [ -f "/home/root/xovi/inactive-extensions/$ext" ] &&
        mv "/home/root/xovi/inactive-extensions/$ext" /home/root/xovi/extensions.d/
done
ls /home/root/xovi/extensions.d/
```

## 4. Hashtable for this exact firmware

**Hashtables are not portable across firmware versions.** A version you
have not seen before needs its own, built on-device:

```sh
ssh -t root@<host> '/home/root/xovi/rebuild_hashtable'
```

This runs xochitl once and **needs a human to unlock the tablet** when
prompted. It also stops xochitl, so `systemctl start xochitl` after.
Cache the result (`/home/root/xovi/exthome/qt-resource-rebuilder/hashtab`)
per firmware version; later installs onto the same version just copy it
back.

## 5. Install the QML patch

```sh
scp xovi/weread.qmd root@<host>:/home/root/xovi/exthome/qt-resource-rebuilder/
```

On 3.27.3.0 the shipped QML is not identifier-obfuscated and the
plaintext `.qmd` applies as-is. If a later firmware obfuscates it, hash
the file first with [`qmldiff`](https://github.com/asivery/qmldiff):
`qmldiff hash-diffs <hashtab> weread.qmd`.

Two traps in the `.qmd` itself:

- The grammar rejects top-level `//` comments — every comment must live
  inside the `INSERT` block. It fails silently.
- `TRAVERSE` matches a file's **root object type**. `SceneViewGestures.qml`
  is a `TouchArea`, which is the only injection point this project uses.
  The library's own QML files have custom root types that cannot be
  guessed, and xochitl's QML is compiled (grep the binary: `ContentHeader`
  isn't in it), so they cannot be read off the device either. That is
  why the shelf browser is reached through a document ("＋ 书架") rather
  than a button in the library.

## 6. Daemon and systemd — on the persistent filesystem

```sh
scp weread_daemon-<arch> root@<host>:/home/root/weread_daemon
ssh root@<host> 'chmod +x /home/root/weread_daemon; mkdir -p /home/root/xovi/exthome/weread'
```

**`/etc` is an overlay whose upper layer is `/var/volatile` — tmpfs.**
Everything written to `/etc/systemd/system` is lost on reboot, including
the symlink `systemctl enable` creates. An install that puts units there
appears to work until the first reboot, then the device comes up with no
daemon and unpatched xochitl. Put them on the read-only root instead:

```sh
scp xovi/weread-daemon.service xovi/xovi-start.service root@<host>:/tmp/
ssh root@<host> '
  mount -o remount,rw /
  mkdir -p /usr/lib/systemd/system/multi-user.target.wants
  for u in weread-daemon.service xovi-start.service; do
      cp /tmp/$u /usr/lib/systemd/system/$u
      ln -sf ../$u /usr/lib/systemd/system/multi-user.target.wants/$u
      rm -f /etc/systemd/system/$u /etc/systemd/system/multi-user.target.wants/$u
  done
  sync; mount -o remount,ro /
  systemctl daemon-reload
'
```

`systemctl is-enabled` will still say `disabled` — it only counts
symlinks under `/etc`. The authoritative check is:

```sh
ssh root@<host> 'systemctl show -p Wants multi-user.target | tr " " "\n" | grep -E "weread|xovi"'
```

Run the daemon under systemd, never from an SSH pipe: its stdout on a
tty that later disappears fills the pipe buffer and a blocked `println!`
deadlocks it.

Fonts need no step. The daemon writes its embedded CJK face to
`exthome/weread/NotoSansSC.ttf` for the popup, and drops a
`/home/root/.config/fontconfig/fonts.conf` (only if absent) pointing at
it — without which xochitl renders every Chinese document name as tofu,
since the device ships Latin Noto only.

## 7. Validate — and know what each check can prove

Start it the way it will actually run:

```sh
ssh root@<host> 'systemctl start xochitl'
```

**Do not validate with `xovi/debug`.** It sets `LD_PRELOAD` but *not*
`XOVI_ROOT`, so the native extensions are not loaded, `CommandExecutor`
is an unknown type, and the entire injected block fails to instantiate —
silently. qmldiff still reports "Processing file …SceneViewGestures.qml"
and the log shows zero errors, so it looks perfect and nothing works.
Hours went into that. `xovi/debug` is still useful for one thing: a
foreground run that shows fatal QML *syntax* errors without risking a
boot loop. It cannot show you that the feature works.

Checks that mean something, under the service:

```sh
ssh root@<host> '
  journalctl -u xochitl -b --no-pager | grep -i qmldiff | grep -v worker | tail -5
  journalctl -u xochitl -b --no-pager | grep -icE "Syntax error|is not a type|Unable to assign|binding loop"
'
```

Expect `Loading file weread.qmd`, `Processing file
/qml/device/view/documentview/SceneViewGestures.qml`, and a count of 0.
`Experimental.qml:76: ReferenceError: Values is not defined` is stock
xochitl noise.

**QML ids resolve at call time.** A deleted or misspelled id is a runtime
`ReferenceError` on a line that may only run mid-generation — it will
not show up at load. After editing the `.qmd`, audit it:

```sh
python3 - <<'EOF'
import re
s = open("xovi/weread.qmd").read()
declared = set(re.findall(r'id:\s*(weRead\w+)', s))
used = set(re.findall(r'\b(weRead[A-Za-z]+)\s*\.', s))
print("referenced but not declared:", sorted(u for u in used if u not in declared) or "none")
EOF
```

The patch only exists while a **document is open** — it is injected into
the document view. Nothing instantiates on the library screen, so "no
log output" there means nothing at all.

## 8. Log in

Login runs on a workstation; it prints a URL to turn into a QR code.

```sh
cargo run --bin weread_login          # writes ./session.json
scp session.json root@<host>:/home/root/.local/share/rm-weread/session.json
ssh root@<host> 'systemctl restart weread-daemon'
```

The account must first enable the WeRead Skill in the phone app
(我 → 设置 → 微信读书 Skill → 获取 API Key). Without it there is no
`apikey` and the shelf call fails — an account switch, not a bug.
`session.json` holds live cookies and that key; it is gitignored and
must never be committed.

## 9. Smoke test without touching the device

```sh
ssh root@<host> 'touch /home/root/xovi/exthome/weread/shelf; sleep 12; cat /home/root/xovi/exthome/weread/shelf.txt'
ssh root@<host> 'touch /home/root/xovi/exthome/weread/generate'
```

Poll the **sequence number** on line 1 of `gen.txt`, not the status word
on line 2 — that still holds the previous run's `done` and will make a
naive loop exit immediately.

## Using it

Four-finger tap inside any open document, or open the **＋ 书架**
document in the 微信读书 folder, to get the shelf browser. Pick a book;
a progress card tracks it. New documents need an xochitl restart to
appear, which the card offers as a button.

Touch the underlined words in a generated book to see other readers'
comments.

## IPC

QML can only run `/bin/touch`, so parameters live in filenames. All
paths are under `/home/root/xovi/exthome/weread/`.

| Trigger | Meaning |
|---|---|
| `shelf` | re-read the shelf → `shelf.json`, `shelf.txt`, `covers/` |
| `generate` | generate the first shelf book |
| `gen_<bookId>_<nonce>` | generate that book |
| `hot_<bookId>_<chapterUid>_<nonce>` | fetch that chapter's underlines → `hot/<bookId>_<chapterUid>.json` |
| `ask_<bookId>_<chapterUid>_<range>_<nonce>` | fetch that passage's reviews → `reviews.txt` |
| `restart` | restart xochitl so a new document appears |

Result files carry `seq / status / body`. `gen.txt`'s status is
`working`, `done`, `done_restart` (a new document exists) or `error`.

## What is installed where

| Path | What |
|---|---|
| `/home/root/weread_daemon` | the binary |
| `/usr/lib/systemd/system/weread-daemon.service`, `xovi-start.service` | units, on the persistent root |
| `/home/root/xovi/exthome/qt-resource-rebuilder/weread.qmd` | QML patch |
| `/home/root/xovi/exthome/weread/` | IPC, `layout/<docUuid>.json`, `hot/`, `covers/`, the font |
| `/home/root/.local/share/rm-weread/` | `session.json`, chapter cache, per-book layouts, `docs.json` |
| `/home/root/.local/share/remarkable/xochitl/` | the generated documents |

## Updating

- **Daemon only**: stop, replace the binary, start. No xochitl restart.
- **QML only**: replace the `.qmd`, run the id audit, `systemctl restart
  xochitl`. A `.qmd` change needs the restart to take effect.
- **Regenerating a book**: safe for existing ink as long as the chapter
  text, the grid constants and `LAYOUT_ALGO_VERSION` are unchanged —
  then `content_sha256` matches and the PDF is swapped in place. If it
  changes and the document already has `.rm` ink, the pipeline refuses
  to overwrite and creates a "(更新版)" sibling. Never work around that.

## Troubleshooting

- **Everything loads, nothing works** — you are almost certainly under
  `xovi/debug`. See step 7.
- **Worked before a reboot, gone after** — the units were in `/etc`.
  See step 6.
- **Chinese document names are tofu** — the device ships no CJK font;
  the daemon's `fonts.conf` handles it, but only if it was allowed to
  create the file.
- **A new book doesn't appear** — xochitl reads the library once at
  startup: no inotify watch on the document directory, no D-Bus call to
  rescan (the only sync interface drives *cloud* sync, and a forged
  `batchFinished` provokes no reload). It genuinely needs a restart.
- **A deletion came back** — xochitl holds the move to trash in memory
  and flushes later. Restarting it before that loses the deletion.
  Deleting means `parent: "trash"`; `deleted` stays `false`.
- **HTTP 499, then a gateway-wide 403 with an empty body** — throttling.
  Generation no longer fetches underlines, so this should not recur; if
  it does, note that the same key works from `wget` on the same device
  at the same moment, i.e. the block is on the client, not the account.
- **After a firmware update** — the hashtab is invalid (step 4), and any
  QML property the patch relies on may have moved. There is no QML on
  disk; re-derive names by walking the parent chain from inside the
  patch and logging property names. Under the service, `console.log`
  reaches journald.
