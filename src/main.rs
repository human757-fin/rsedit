#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

use crate::app::FastCutterApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("Fast Cutter"),
        ..Default::default()
    };
    eframe::run_native(
        "Fast Cutter",
        options,
        Box::new(|cc| {
            app::apply_theme(&cc.egui_ctx);
            Ok(Box::new(FastCutterApp::default()))
        }),
    )
}