// Release builds are a windowed app on Windows: no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cdp;
mod engine;
mod keys;
mod look;
mod omnibox;

use eframe::egui;

fn main() -> eframe::Result {
    let initial_url = std::env::args().nth(1).and_then(|arg| omnibox::resolve(&arg));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Obscura")
            .with_app_id("obscura")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([480.0, 320.0])
            // The tab strip is the title bar, as in Chromium-style browsers.
            .with_decorations(false),
        ..Default::default()
    };
    eframe::run_native(
        "Obscura",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(&cc.egui_ctx, initial_url)))),
    )
}
