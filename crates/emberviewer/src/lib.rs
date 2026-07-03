//! Ember+ viewer. The native desktop app (`app`) and the wasm browser client
//! (`web`) share the protocol (`ember-proto`), the tree model (`model`), and the
//! command/event vocabulary (`net`, `wire`); their UIs and transports differ.

#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
mod address_book;
#[cfg(not(target_arch = "wasm32"))]
mod app;
#[cfg(not(target_arch = "wasm32"))]
mod debug_log;
#[cfg(not(target_arch = "wasm32"))]
mod discovery;
#[cfg(not(target_arch = "wasm32"))]
mod fsutil;
#[cfg(not(target_arch = "wasm32"))]
mod hub;
mod matrix_view;
mod model;
mod net;
#[cfg(not(target_arch = "wasm32"))]
mod server;
#[cfg(not(target_arch = "wasm32"))]
mod settings;
#[cfg(not(target_arch = "wasm32"))]
mod update;
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
mod web_transport;
mod widgets;
// Some conversions here are server-side only and unused in the wasm build.
#[allow(dead_code)]
mod wire;

/// Launch the native desktop application.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() -> eframe::Result {
    // Seed frame dumping from EMBER_DUMP (developer workflow), then install the
    // tracing subscriber: console output plus an off-by-default file layer that
    // the GUI's "Enable debug log" option turns on.
    ember_net::init_frame_dump_from_env();
    debug_log::init(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    );

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([900.0, 640.0])
        .with_title("emberviewer");
    // Window/taskbar icon (all platforms). On Windows the .exe also carries an
    // embedded icon resource (see build.rs) for Explorer and the taskbar.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "emberviewer",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// Browser entry point: mount the wasm UI on the `<canvas id="the_canvas_id">`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start_web() {
    use wasm_bindgen::JsCast;
    console_error_panic_hook::set_once();
    wasm_bindgen_futures::spawn_local(async {
        let canvas = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("the_canvas_id"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("a <canvas id=\"the_canvas_id\"> element");
        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(web::WebApp::new(cc)))),
            )
            .await
            .expect("failed to start eframe web");
    });
}
