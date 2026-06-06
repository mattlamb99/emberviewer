# emberviewer

A cross-platform desktop viewer for the **Ember+** control protocol (Lawo) — an open,
maintained replacement for Lawo's closed-source, Windows-only `EmberPlusView.exe`.

Keep an address book of Ember+ providers organised in folders, connect over TCP, browse a
provider's tree, view & set parameter values with live subscriptions, route matrices, invoke
functions, and watch streamed meters. One Rust codebase targeting **Windows, macOS and Linux**.

**New: [server mode](#server-mode-browser-access)** — flip a switch and operate the running
desktop app from any phone or laptop browser on your network, without those devices needing
direct access to the Ember+ gear.

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
- **Server mode (browser access)** — *new:* serve the UI to phones and laptops on your LAN
  (see [below](#server-mode-browser-access)). Token-protected by default, with a read-only
  toggle and an open-LAN mode; a copyable URL and a QR code make joining a one-tap affair.
- **Discovery** — find providers on the LAN via mDNS (`_ember._tcp`).
- **Logging** — per-parameter change log to a window, optionally appended to a file.
- **Theming** — dark / light toggle (persisted) with a warm accent; offline subtrees grey out.
- **Robust transport** — auto keep-alive, reconnect with backoff, multi-package S101
  reassembly, and a lenient decoder that tolerates vendor extension fields.

## Server mode (browser access)

Ember+ gear usually lives behind a firewall that only the engineering PC can reach, and the
embedded devices themselves often cap how many consumers may connect. Server mode turns the
desktop app into that single gateway: it holds **one** connection per provider and fans the live
tree out to every viewer — the local window **and** any number of browsers — so you can route a
matrix or check a meter from a phone on the WiFi without giving that phone access to the gear.

```
Ember+ devices ──TCP──► emberviewer (engineering PC)  ──HTTP/WebSocket──►  📱 browser
                         one connection per device       shared live tree   💻 browser
```

- **One consumer per device.** A fan-out hub shares a single TCP connection and one subscription
  set across all viewers, so the device sees just one consumer no matter how many phones are
  watching. Closing a browser never drops another viewer's updates.
- **Browsers stay thin.** The browser runs the same egui UI compiled to **WebAssembly**; it never
  talks to the devices directly. Documents cross the WebSocket as the device's original Glow/BER
  bytes (forwarded verbatim, not re-encoded), so a browser rebuilds a byte-identical tree.
- **Access control.** A shared **token** is required by default (it travels in the page URL).
  Toggle **open on LAN** for no-auth access on a trusted network, or **read-only** to let
  browsers watch but not change values / routes / invoke functions.
- **Easy joining.** Pick which network interface to bind, then share the shown
  `http://<lan-ip>:<port>/?token=…` URL — or just scan the QR code.

**Enable it:** *Options → Server mode* → tick **Enable**, choose a port (default `8080`) and
interface, then open the URL on another device. The web UI mirrors one provider at a time and
supports browsing, live values, meters, matrices, functions and the signal-parameters popup. For
a binary you build yourself, build the web bundle first so it gets embedded (see
[Building](#building)); the published release binaries already include it.

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

### The web bundle (for server mode)

[Server mode](#server-mode-browser-access) embeds a WebAssembly build of the UI. A plain
`cargo build` serves a "bundle not built" placeholder; to embed the real browser UI, build the
bundle first, then build the app:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version "$(cargo metadata --format-version=1 --locked \
  | jq -r '.packages[] | select(.name=="wasm-bindgen") | .version' | head -1)"  # must match the crate
./scripts/build-web.sh          # → crates/emberviewer/web-dist/ (gitignored, embedded next build)
cargo build --release -p emberviewer
```

CI does this for you: the `web-bundle` job in `release.yml` builds the bundle once and the three
native release jobs download and embed it, so **published release binaries already ship the web
UI** — you only need the steps above when building your own.

## Architecture

A cargo workspace with the protocol cleanly separated from the UI:

| Crate | Role |
|-------|------|
| `ember-proto` | Pure protocol, no I/O: **S101** framing (CRC-16/X-25, escaping, keep-alive, multi-package reassembly) and the **Glow** schema encoded as **BER** via [`rasn`]. Compiles to wasm unchanged, so the browser decodes documents with the same code. |
| `ember-net` | Async [`tokio`] TCP transport — connect, send/receive Glow documents, auto keep-alive. Native, server-side only. |
| `ember-web-proto` | The WebSocket wire vocabulary for server mode: small JSON control messages + the binary document framing shared by the server and the wasm client. |
| `emberviewer` | The app: address book + tree browser + matrices/functions/streams, built with [`egui`]/`eframe`. Compiles to a **native** desktop binary (with an embedded [`axum`] server for [server mode](#server-mode-browser-access)) and to a **wasm** browser client from one source. |

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
[`axum`]: https://github.com/tokio-rs/axum
