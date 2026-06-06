//! Ember+ viewer desktop application.

// A complete address-book API; not every method is wired into the UI yet.
#[allow(dead_code)]
mod address_book;
mod app;
mod model;
mod net;
mod settings;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 640.0])
            .with_title("emberviewer"),
        ..Default::default()
    };

    eframe::run_native(
        "emberviewer",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
