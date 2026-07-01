//! Ensure the embedded web-bundle folder exists so `rust-embed` compiles even
//! when the wasm UI hasn't been built (e.g. a plain `cargo build`). The real
//! bundle (emberviewer.js + emberviewer_bg.wasm + index.html) is produced
//! separately via wasm-bindgen and overwrites the placeholder.

use std::path::Path;

fn main() {
    let dist = Path::new("web-dist");
    let index = dist.join("index.html");
    if !index.exists() {
        let _ = std::fs::create_dir_all(dist);
        let _ = std::fs::write(
            &index,
            "<!doctype html><meta charset=utf-8><title>emberviewer</title>\
             <body style=\"font-family:system-ui;margin:3rem\">\
             <h1>emberviewer</h1>\
             <p>The browser UI bundle was not built. Build it with wasm-bindgen \
             (see the README) so server mode can serve it.</p>",
        );
    }
    println!("cargo:rerun-if-changed=web-dist/index.html");

    // Embed the application icon into the Windows .exe resource so Explorer, the
    // title bar, and the taskbar show it. Gated on the *target* OS so it also runs
    // when cross-compiling to x86_64-pc-windows-gnu from Linux (winresource drives
    // the mingw resource compiler). No-op on other targets.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=failed to embed Windows icon: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
