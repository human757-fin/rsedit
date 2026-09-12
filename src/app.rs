use eframe::egui;

const BG: egui::Color32 = egui::Color32::from_rgb(8, 8, 10);
const BG_PANEL: egui::Color32 = egui::Color32::from_rgb(14, 14, 17);
const BG_SELECT: egui::Color32 = egui::Color32::from_rgb(24, 24, 29);
const FG: egui::Color32 = egui::Color32::from_rgb(236, 236, 240);
const FG_DIM: egui::Color32 = egui::Color32::from_rgb(150, 150, 158);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(240, 240, 245);

pub fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = BG_PANEL;
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = BG_PANEL;
    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.fg_stroke.color = FG_DIM;
    visuals.widgets.inactive.bg_fill = BG_SELECT;
    visuals.widgets.inactive.fg_stroke.color = FG;
    visuals.widgets.hovered.bg_fill = BG_SELECT;
    visuals.widgets.hovered.fg_stroke.color = ACCENT;
    visuals.widgets.active.bg_fill = BG_SELECT;
    visuals.widgets.active.fg_stroke.color = ACCENT;
    visuals.widgets.open.bg_fill = BG_SELECT;
    visuals.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(240, 240, 245, 24);
    visuals.selection.stroke.color = ACCENT;
    ctx.set_visuals(visuals);
}

pub struct FastCutterApp {
    version: &'static str,
}

impl Default for FastCutterApp {
    fn default() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

impl eframe::App for FastCutterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.4);
                ui.label(
                    egui::RichText::new("FAST CUTTER")
                        .size(48.0)
                        .strong()
                        .color(FG),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("a minimal, fast, cross-platform video editor")
                        .size(14.0)
                        .color(FG_DIM),
                );
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(format!("v{}", self.version))
                        .size(12.0)
                        .monospace()
                        .color(FG_DIM),
                );
            });
        });
    }
}