//! Native launcher. The application lives in the library crate (`emberviewer`),
//! which is shared with the wasm browser client.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    emberviewer::run_native()
}

// On wasm there is no native binary; the entry point is the library's
// `start_web` (`#[wasm_bindgen(start)]`).
#[cfg(target_arch = "wasm32")]
fn main() {}
