# rm-weread

A native 微信读书 (WeRead) client for reMarkable tablets (reMarkable 2 and
Paper Pro), built as a single Rust binary + QML — no companion app, no
cloud account of our own, no backend to maintain. WeRead's own servers
are the only backend; this project is a thin client on top of them.

Design and rationale: [docs/design.md](docs/design.md).

Separate from [`rm-agent`](../rm-agent) (this device's other project) —
different concern, different lifecycle, sized like
[`inkwell-suite`](https://github.com/JIACHENG135/inkwell-suite): small,
focused, one thing done well.

## Status

Early design/scaffolding stage — no working binary yet. See
[docs/design.md](docs/design.md) for the phased plan.

## Building

Cross-compiled for reMarkable targets (see
[`.cargo/config.toml`](.cargo/config.toml)):

```sh
cargo build --release --target armv7-unknown-linux-musleabihf   # reMarkable 2
cargo build --release --target aarch64-unknown-linux-musl        # Paper Pro
```
