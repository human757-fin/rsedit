#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assets;
mod audio;
mod decoder;
mod export;
mod fonts;
mod framecache;
mod render;
mod text;
mod timeline;

use crate::app::FastCutterApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("Fast Cutter"),
        ..Default::default()
    };
    eframe::run_native(
        "Fast Cutter",
        options,
        Box::new(|cc| Ok(Box::new(FastCutterApp::new(cc)))),
    )
}