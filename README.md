# emberviewer

A cross-platform desktop viewer for the **Ember+** control protocol (Lawo) - an open, maintained
replacement for Lawo's closed-source, Windows-only `EmberPlusView.exe`. One Rust codebase for
**Windows, macOS and Linux**.

Keep an address book of providers, browse a provider's tree, view & set parameters with live
subscriptions, route matrices, invoke functions, and watch streamed meters - plus a
**[server mode](#server-mode)** to operate the running desktop app from any phone or laptop browser
on your network.

> **Status: usable.** Tested against real broadcast gear - Lawo (Power Core, mc²36, VSM, Virtual
> Patch Bay, vPro8, Gadget Server, DMS), Riedel MicroN, Arkona AT300, DirectOut Maven, 2wcom RF10e,
> Tieline Gateway - and the [`node-emberplus`](https://github.com/evs-broadcast/node-emberplus) stack.

## Why

`EmberPlusView.exe` is the de-facto tool for Ember+, but its source was never published
([Lawo/ember-plus#59](https://github.com/Lawo/ember-plus/issues/59)), it's Windows-binary-only, and
it hasn't been updated since 2022. emberviewer reimplements an equivalent - cross-platform, from the
protocol up, with quality-of-life additions.

## Features

- **Address book** - providers in folders, drag-and-drop, JSON persistence; optional auto-connect on startup.
- **Browse** - lazy `getDirectory` tree walking, filter by identifier, sort, copy path/identifier.
- **View & set** - type-aware editors (int, real, string, bool, trigger, enum); sliders honour min/max, factor and printf `format`; read-only vs read-write badged.
- **Live updates** - visible parameters auto-subscribe and reflect pushed changes; collapsing unsubscribes.
- **Matrices** - crosspoint grid with device-resolved source/target labels, click to route, configurable orientation, and a signal-parameters popup.
- **Functions** - argument form, invoke, rendered result.
- **Streams / meters** - `StreamFormat`-aware decode; a vertical meter for the selected parameter, plus pop-out windows.
- **Server mode** - serve the UI to phones/laptops on your LAN; token-protected, with read-only and open-LAN modes (see below).
- **Discovery** - find providers via mDNS (`_ember._tcp`).
- **More** - per-parameter change logging (window or file), dark/light theme, an opt-in safety lock against accidental edits, a TX/RX traffic counter, and robust transport (keep-alive, reconnect with backoff, multi-package S101 reassembly).

## Server mode

Ember+ gear usually sits behind a firewall only the engineering PC can reach, and embedded devices
cap how many consumers may connect. Server mode makes the desktop app the single gateway: it holds
**one** connection per provider and fans the live tree out to every viewer - the local window and any
number of browsers.

```
Ember+ devices ──TCP──► emberviewer (engineering PC) ──HTTP/WebSocket──► 📱💻 browsers
                        one connection per device      shared live tree
```

Browsers run the same egui UI compiled to WebAssembly and never touch the devices directly;
documents cross the WebSocket as the device's original Glow/BER bytes. A shared **token** is required
by default (toggle **open on LAN** or **read-only** as needed), and a copyable URL + QR code make
joining one-tap.

**Enable it:** *Options → Server mode* → **Enable**, pick a port (default `8080`) and interface, then
open the URL on another device. Published release binaries already embed the web bundle; building it
yourself needs one extra step (see [Building](#building)).

## Install

Grab a build from [Releases](https://github.com/mattlamb99/emberviewer/releases) (Windows `.zip`,
macOS/Linux `.tar.gz`). On first launch Windows may warn about an unsigned binary - *More info → Run
anyway*. Or build from source.

## Building

Requires a [Rust toolchain](https://rustup.rs).

```sh
cargo build --release --workspace
cargo test  --workspace      # unit + interop tests against captured real frames
cargo run   -p emberviewer
```

On Debian/Ubuntu the GUI needs:

```sh
sudo apt install libgtk-3-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
                 libxkbcommon-dev libgl1-mesa-dev
```

**Web bundle (for server mode):** a plain build serves a placeholder; to embed the real browser UI,
build the wasm bundle first:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <wasm-bindgen crate version>   # versions must match
./scripts/build-web.sh          # → crates/emberviewer/web-dist/ (gitignored)
cargo build --release -p emberviewer
```

CI does this automatically, so **published release binaries already ship the web UI**.

## Architecture

A cargo workspace with the protocol cleanly separated from the UI:

| Crate | Role |
|-------|------|
| `ember-proto` | Pure protocol, no I/O: **S101** framing and the **Glow** schema as **BER** via [`rasn`]. Compiles to wasm unchanged. |
| `ember-net` | Async [`tokio`] TCP transport. Native, server-side only. |
| `ember-web-proto` | The WebSocket wire vocabulary for server mode (JSON control + binary document framing). |
| `emberviewer` | The app ([`egui`]/`eframe`): a native desktop binary (with an embedded [`axum`] server) and a wasm browser client, from one source. |

The wire format is three nested layers over TCP (default port **9000**):

```
TCP → S101 frames → BER (TLV) → Glow tree (nodes, parameters, matrices, functions, streams)
```

Implemented from scratch (no mature Ember+ library exists in Rust); a few non-obvious details -
ASN.1 `REAL` following libember's IEEE-exponent convention, `RELATIVE-OID` paths, the outer
`[APPLICATION 0]` Root wrapper - are handled in `ember-proto/src/glow.rs` and covered by tests.

## Test provider

A small [`node-emberplus`](https://github.com/evs-broadcast/node-emberplus) provider lives in
`testprovider/` (one parameter of each type, matrices, a function, streamed meters):

```sh
cd testprovider && npm install && node server.js     # listens on 0.0.0.0:9000
```

Point the app at `127.0.0.1:9000`, or walk the tree headlessly:

```sh
cargo run -p ember-net --example walk -- 127.0.0.1:9000
```

## Contributing

Issues and PRs welcome. CI runs `cargo fmt --check`, `clippy -D warnings`, and the test suite across
Linux/macOS/Windows; please keep those green.

## License

[MIT](LICENSE).

[`rasn`]: https://github.com/librasn/rasn
[`tokio`]: https://tokio.rs
[`egui`]: https://github.com/emilk/egui
[`axum`]: https://github.com/tokio-rs/axum
