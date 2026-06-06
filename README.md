# emberviewer

A cross-platform desktop viewer for the **Ember+** control protocol (Lawo) — an open,
maintained replacement for Lawo's closed-source, Windows-only `EmberPlusView.exe`.

Keep an address book of Ember+ providers organised in folders, connect over TCP, browse a
provider's tree, and view & set parameter values. Targets Windows, macOS and Linux from a
single Rust codebase.

> Status: **early development.** The protocol stack and a headless tree walker work and are
> validated against a live provider. The GUI is being built.

## Why

`EmberPlusView.exe` is the de-facto tool for poking at Ember+ devices, but its source was
never published ([Lawo/ember-plus#59](https://github.com/Lawo/ember-plus/issues/59)), it ships
Windows-binary-only, and it hasn't been updated since 2022. This project reimplements an
equivalent, cross-platform, from the protocol up.

## Architecture

A cargo workspace with the protocol cleanly separated from the UI:

| Crate | Role |
|-------|------|
| `ember-proto` | Pure protocol, no I/O: **S101** framing (CRC-16/X-25, escaping, keep-alive, multi-package reassembly) and the **Glow** schema encoded as **BER** via [`rasn`]. |
| `ember-net` | Async [`tokio`] TCP transport — connect, send/receive Glow documents, auto keep-alive. |
| `emberviewer` | The desktop app: address book + tree browser, built with [`egui`]/`eframe`. |

The Ember+ wire format is three nested layers over TCP (default port **9000**):

```
TCP  →  S101 frames  →  BER (TLV)  →  Glow tree (nodes, parameters, matrices, functions, streams)
```

The protocol is implemented from scratch (no mature Ember+ library exists in Rust). A few
non-obvious details — ASN.1 `REAL` follows libember's IEEE-exponent convention rather than
textbook `M × 2^E`, paths are `RELATIVE-OID`, and every message is wrapped in an outer
`[APPLICATION 0]` Root — are handled in `ember-proto/src/glow.rs` and covered by tests.

## Building

Requires a [Rust toolchain](https://rustup.rs).

```sh
cargo build --workspace
cargo test  --workspace      # unit tests + interop tests against captured real frames
```

## Trying it against a test provider

A small [`node-emberplus`](https://github.com/evs-broadcast/node-emberplus) provider lives in
`testprovider/` (serves a sample tree with one parameter of each type):

```sh
cd testprovider && npm install && node server.js     # listens on 0.0.0.0:9000
```

Then walk its tree headlessly to verify the stack:

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

## Roadmap

- [x] S101 framing + Glow BER types, validated against real provider frames
- [x] Async TCP transport + headless tree walk
- [x] Address book (folders of providers, JSON persistence)
- [ ] egui GUI: address book sidebar + lazy tree browser with a value column
- [ ] Get/set parameter values + live change subscriptions
- [ ] Reconnect/robustness, search, multiple connections
- [ ] Matrices (crosspoints) + functions (invocation)
- [ ] High-rate streams (meters) + mDNS provider discovery

## License

MIT.

[`rasn`]: https://github.com/librasn/rasn
[`tokio`]: https://tokio.rs
[`egui`]: https://github.com/emilk/egui
