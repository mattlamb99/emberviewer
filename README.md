# emberviewer

A cross-platform desktop viewer for the **Ember+** control protocol (Lawo) — an open,
maintained replacement for Lawo's closed-source, Windows-only `EmberPlusView.exe`.

Keep an address book of Ember+ providers organised in folders, connect over TCP, browse a
provider's tree, view & set parameter values with live subscriptions, route matrices, invoke
functions, and watch streamed meters. One Rust codebase targeting **Windows, macOS and Linux**.

> Status: **usable.** The protocol stack is validated against frames from a live provider, and
> the GUI implements browsing, get/set, matrices, functions, streams and discovery. Tested
> against [`node-emberplus`](https://github.com/evs-broadcast/node-emberplus) and a real Lawo
> Ruby device.

## Why

`EmberPlusView.exe` is the de-facto tool for poking at Ember+ devices, but its source was
never published ([Lawo/ember-plus#59](https://github.com/Lawo/ember-plus/issues/59)), it ships
Windows-binary-only, and it hasn't been updated since 2022. This project reimplements an
equivalent, cross-platform, from the protocol up, with a few quality-of-life additions.

## Features

- **Address book** — providers organised in folders, drag-and-drop to rearrange, JSON
  persistence in your OS config dir. Optionally auto-connect all / last session on startup.
- **Browse** — lazy `getDirectory` tree walking; expand a node to fetch its children. Filter
  by identifier, sort by number / identifier / description, copy a path or identifier.
- **View & set parameters** — type-aware editors for integer, real, string, boolean, trigger,
  and enum parameters; sliders honour the provider's min/max, display factor and printf
  `format`; enums use `enumeration`/`enumMap` (with `~`-hidden entries). Right-click → **Copy
  value**. Read-only vs read-write is badged.
- **Live updates** — visible parameters are subscribed automatically and reflect pushed value
  changes in real time; collapsing unsubscribes.
- **Matrices** — crosspoint grid with source/target **labels** resolved from the device,
  rotated column headers, a resizable label column, click to route/clear, configurable
  orientation (targets-on-top or sources-on-top), and a **signal-parameters** popup (gain,
  type, name, …) when you click a row/column header.
- **Functions** — argument form, invoke, and rendered `InvocationResult`.
- **Streams / meters** — high-rate `StreamFormat`-aware decode; a vertical meter for the
  selected parameter, pop-out meter windows, and an always-on-top toggle.
- **Discovery** — find providers on the LAN via mDNS (`_ember._tcp`).
- **Logging** — per-parameter change log to a window, optionally appended to a file.
- **Theming** — dark / light toggle (persisted) with a warm accent; offline subtrees grey out.
- **Robust transport** — auto keep-alive, reconnect with backoff, multi-package S101
  reassembly, and a lenient decoder that tolerates vendor extension fields.

## Install

Grab a build for your platform from the
[Releases](https://github.com/mattlamb99/emberviewer/releases) page (Windows `.zip`, macOS and
Linux `.tar.gz`). On first launch Windows may warn about an unsigned binary — *More info →
Run anyway*.

Or build from source (see below).

## Building

Requires a [Rust toolchain](https://rustup.rs).

```sh
cargo build --release --workspace
cargo test  --workspace      # unit tests + interop tests against captured real frames
cargo run   -p emberviewer   # launch the app
```

On Linux the GUI needs the usual `egui`/`winit` system libraries; on Debian/Ubuntu:

```sh
sudo apt install libgtk-3-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
                 libxkbcommon-dev libgl1-mesa-dev
```

## Architecture

A cargo workspace with the protocol cleanly separated from the UI:

| Crate | Role |
|-------|------|
| `ember-proto` | Pure protocol, no I/O: **S101** framing (CRC-16/X-25, escaping, keep-alive, multi-package reassembly) and the **Glow** schema encoded as **BER** via [`rasn`]. |
| `ember-net` | Async [`tokio`] TCP transport — connect, send/receive Glow documents, auto keep-alive. |
| `emberviewer` | The desktop app: address book + tree browser + matrices/functions/streams, built with [`egui`]/`eframe`. |

The Ember+ wire format is three nested layers over TCP (default port **9000**):

```
TCP  →  S101 frames  →  BER (TLV)  →  Glow tree (nodes, parameters, matrices, functions, streams)
```

The protocol is implemented from scratch (no mature Ember+ library exists in Rust). A few
non-obvious details — ASN.1 `REAL` follows libember's IEEE-exponent convention rather than
textbook `M × 2^E`, paths are `RELATIVE-OID`, and every message is wrapped in an outer
`[APPLICATION 0]` Root — are handled in `ember-proto/src/glow.rs` and covered by tests.

## Trying it against a test provider

A small [`node-emberplus`](https://github.com/evs-broadcast/node-emberplus) provider lives in
`testprovider/` (serves a sample tree: one parameter of each type, matrices, a function, and
streamed meters):

```sh
cd testprovider && npm install && node server.js     # listens on 0.0.0.0:9000
```

Then point the app at `127.0.0.1:9000`, or walk the tree headlessly to verify the stack:

```sh
cargo run -p ember-net --example walk -- 127.0.0.1:9000
```

```
0  [node] EmberViewerTestProvider
  0.1  [node] parameters
    0.1.0  intParam = 42 (rw)
    0.1.1  realParam = 3.141590 (ro)
    0.1.4  enumParam = 1 (rw)
```

## Contributing

Issues and PRs welcome. CI runs `cargo fmt --check`, `clippy -D warnings`, and the test suite
across Linux/macOS/Windows; please keep those green.

## License

[MIT](LICENSE).

[`rasn`]: https://github.com/librasn/rasn
[`tokio`]: https://tokio.rs
[`egui`]: https://github.com/emilk/egui
