//! Fast Cutter — the editor application itself.
//!
//! Layout: top transport bar, left media panel, center preview viewport,
//! right clip inspector, bottom multi-track timeline. Theme is near-black
//! (#08080a) with near-white foreground, per the design brief.

use std::path::PathBuf;

use eframe::egui;
use egui::{Color32, RichText};

use crate::audio::{AudioEngine, timecode_string};
use crate::decoder::ensure_ffmpeg;
use crate::framecache::{FrameCache, FrameRequest};
use crate::timeline::{
    AssetId, AssetKind, AudioClip, Project, TextAlign, TextClip, TextStyle, Timecode, Transform,
    VideoClip, SECOND_US,
};

const PREVIEW_W: u32 = 1280;
const PREVIEW_H: u32 = 720;

pub struct FastCutterApp {
    project: Project,
    cache: FrameCache,
    audio: AudioEngine,
    playhead_us: Timecode,
    playing: bool,
    last_request: Option<(u64, i64)>,
    preview_texture: Option<egui::TextureHandle>,
    preview_rendered_pts: Option<i64>,
    selected: Selection,
    export_open: bool,
    export_path: String,
    export_w: u32,
    export_h: u32,
    export_fps: f64,
    export_msg: Option<String>,
    show_peaks: bool,
    toasts: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
enum Selection {
    None,
    Video { track: usize, clip: usize },
    Audio { track: usize, clip: usize },
    Text { track: usize, clip: usize },
}

impl Default for Selection {
    fn default() -> Self {
        Self::None
    }
}

impl FastCutterApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ensure_ffmpeg();
        apply_theme(&cc.egui_ctx);
        let project = Project::new();
        let audio = AudioEngine::new(&project);
        let cache = FrameCache::new();
        Self {
            project,
            cache,
            audio,
            playhead_us: 0,
            playing: false,
            last_request: None,
            preview_texture: None,
            preview_rendered_pts: None,
            selected: Selection::None,
            export_open: false,
            export_path: "out.mp4".into(),
            export_w: 1920,
            export_h: 1080,
            export_fps: 30.0,
            export_msg: None,
            show_peaks: true,
            toasts: Vec::new(),
        }
    }

    fn compute_preview(&mut self) {
        let dur = self.project.duration_us().max(SECOND_US);
        let pts = self.playhead_us.min(dur);
        let w = PREVIEW_W;
        let h = PREVIEW_H;
        if let Some(tex) = self.preview_texture.as_mut() {
            let rgba = crate::render::Composition {
                project: &self.project,
                cache: &self.cache,
                w,
                h,
            }
            .render_at(pts);
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            tex.set(img, egui::TextureOptions::LINEAR);
        }
        self.preview_rendered_pts = Some(pts);
    }

    fn request_frame(&mut self) {
        // Ask the decoder for the asset under the playhead (topmost video clip).
        let pts = self.playhead_us;
        if let Some((ti, clip)) = top_video_clip(&self.project, pts) {
            if let Some(asset) = self.project.assets.get(clip.asset) {
                if asset.kind == AssetKind::Image {
                    return;
                }
                let key = (asset.id.0, pts);
                if self.last_request != Some(key) {
                    self.cache.set_target(FrameRequest {
                        asset: asset.id.0,
                        path: asset.path.clone(),
                        w: PREVIEW_W,
                        h: PREVIEW_H,
                        pts_us: pts,
                    });
                    self.last_request = Some(key);
                    let _ = ti;
                }
            }
        }
    }
}

fn top_video_clip(p: &Project, t: Timecode) -> Option<(usize, &VideoClip)> {
    p.video_tracks.iter().enumerate().rev().find_map(|(ti, tr)| {
        tr.clips
            .iter()
            .find(|c| c.timeline_start <= t && t < c.timeline_start + (c.source_out - c.source_in))
            .map(|c| (ti, c))
    })
}

impl eframe::App for FastCutterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Transport clock driven by audio engine when playing.
        if self.playing {
            let pos = self.audio.pos_us();
            if pos > self.playhead_us as f64 {
                self.playhead_us = pos as i64;
            }
            if self.playhead_us > self.project.duration_us().max(SECOND_US) {
                self.playhead_us = 0;
                self.audio.set_pos_us(0.0);
                self.playing = false;
                self.audio.pause();
            }
        }

        self.request_frame();
        if self.preview_texture.is_none() {
            let size = [PREVIEW_W as usize, PREVIEW_H as usize];
            let s = ctx.load_texture(
                "preview",
                egui::ColorImage::new(size, Color32::from_rgb(8, 8, 10)),
                egui::TextureOptions::LINEAR,
            );
            self.preview_texture = Some(s);
        }
        if self.preview_rendered_pts != Some(self.playhead_us.min(self.project.duration_us().max(SECOND_US))) {
            self.compute_preview();
        }

        self.top_bar(ctx);
        self.media_panel(ctx);
        self.inspector(ctx);
        self.timeline(ctx);
        if self.export_open {
            self.export_window(ctx);
        }
        self.status_bar(ctx);
        ctx.request_repaint();
    }
}

impl FastCutterApp {
    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("transport").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("◆ Fast Cutter").color(WHITE));
                ui.separator();
                let play = if self.playing {
                    RichText::new("⏸").color(WHITE)
                } else {
                    RichText::new("▶").color(WHITE)
                };
                if ui
                    .add_sized([40.0, 28.0], egui::Button::new(play).fill(BUTTON))
                    .on_hover_text("Space")
                    .clicked()
                {
                    self.toggle_play();
                }
                if ui
                    .add_sized([44.0, 28.0], egui::Button::new("⏮").fill(BUTTON))
                    .clicked()
                {
                    self.playhead_us = 0;
                    self.seek_marker();
                }
                if ui
                    .add_sized([44.0, 28.0], egui::Button::new("⏭").fill(BUTTON))
                    .clicked()
                {
                    self.playhead_us = self.project.duration_us().max(SECOND_US);
                    self.seek_marker();
                }
                ui.label(format!(
                    "{} / {}",
                    timecode_string(self.playhead_us, self.project.fps),
                    timecode_string(self.project.duration_us(), self.project.fps)
                ));
                ui.separator();
                if ui
                    .add_sized([64.0, 28.0], egui::Button::new("Export").fill(ACCENT))
                    .clicked()
                {
                    self.export_open = true;
                }
            });
            ui.add_space(4.0);
        });
    }

    fn toggle_play(&mut self) {
        self.playing = !self.playing;
        if self.playing {
            self.audio.ensure_stream();
            self.audio.set_pos_us(self.playhead_us as f64);
            self.audio.play();
        } else {
            self.audio.pause();
            self.audio.set_pos_us(self.playhead_us as f64);
        }
    }

    fn seek_marker(&mut self) {
        self.audio.set_pos_us(self.playhead_us as f64);
        self.last_request = None;
    }

    fn media_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("media")
            .resizable(true)
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.strong("Media");
                    ui.separator();
                    if ui
                        .add_sized([56.0, 22.0], egui::Button::new("＋ Import").fill(ACCENT))
                        .clicked()
                    {
                        let path = rfd::FileDialog::new()
                            .set_title("Import media")
                            .add_filter(
                                "Media",
                                &["mp4", "mov", "mkv", "webm", "avi", "mp3", "wav", "aac", "flac", "ogg", "png", "jpg", "jpeg", "webp"],
                            )
                            .pick_file();
                        if let Some(p) = path {
                            self.import_media(p);
                        }
                    }
                });
                ui.add_space(4.0);

                let mut remove_id: Option<AssetId> = None;
                let mut add_id: Option<AssetId> = None;
                let ids: Vec<AssetId> = self.project.assets.assets.keys().copied().collect();
                let fps = self.project.fps;
                egui::ScrollArea::vertical().max_height(ui.available_height()).show(ui, |ui| {
                    for id in ids {
                        let Some(a) = self.project.assets.get(id) else { continue };
                        let kind = match a.kind {
                            AssetKind::Video => "🎬",
                            AssetKind::Audio => "🎵",
                            AssetKind::Image => "🖼",
                        };
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "{kind} {}  [{:?}]",
                                a.name,
                                timecode_string(a.duration_us, fps)
                            ));
                            if ui.small_button("➤").clicked() {
                                add_id = Some(id);
                            }
                            if ui.small_button("×").clicked() {
                                remove_id = Some(id);
                            }
                        });
                    }
                });
                if let Some(id) = add_id {
                    self.add_clip_from_media(id);
                }
                if let Some(id) = remove_id {
                    self.remove_asset(id);
                }
                ui.separator();
                ui.checkbox(&mut self.show_peaks, "Show audio peaks");
            });
    }

    fn import_media(&mut self, path: PathBuf) {
        let path_s = path.to_string_lossy().into_owned();
        match crate::assets::import_media(&mut self.project.assets, &path_s) {
            Ok(id) => {
                self.audio.refresh(&self.project);
                self.toasts.push(format!("Imported {}", self.project.assets.get(id).map(|a| a.name.clone()).unwrap_or_default()));
            }
            Err(e) => self.toasts.push(format!("Import failed: {e}")),
        }
    }

    fn remove_asset(&mut self, id: AssetId) {
        // Only drop assets no longer referenced by any clip.
        let used_video = self
            .project
            .video_tracks
            .iter()
            .any(|t| t.clips.iter().any(|c| c.asset == id));
        let used_audio = self
            .project
            .audio_tracks
            .iter()
            .any(|t| t.clips.iter().any(|c| c.asset == id));
        if used_video || used_audio {
            self.toasts.push("Remove clips first".into());
            return;
        }
        self.project.assets.assets.remove(&id);
        self.audio.refresh(&self.project);
    }

    fn add_clip_from_media(&mut self, id: AssetId) {
        let Some(asset) = self.project.assets.get(id).cloned() else { return };
        let start = self.project.duration_us();
        let dur = asset.duration_us.max(SECOND_US);
        match asset.kind {
            AssetKind::Video => {
                self.project.video_tracks[0].clips.push(VideoClip {
                    asset: AssetId(id.0),
                    source_in: 0,
                    source_out: dur,
                    timeline_start: start,
                    opacity: 1.0,
                    transform: Transform::default(),
                });
            }
            AssetKind::Audio => {
                self.project.audio_tracks[0].clips.push(AudioClip {
                    asset: AssetId(id.0),
                    source_in: 0,
                    source_out: dur,
                    timeline_start: start,
                    gain_db: 0.0,
                    fade_in_us: 200_000,
                    fade_out_us: 200_000,
                });
            }
            AssetKind::Image => {
                self.project.video_tracks[0].clips.push(VideoClip {
                    asset: AssetId(id.0),
                    source_in: 0,
                    source_out: SECOND_US * 3,
                    timeline_start: start,
                    opacity: 1.0,
                    transform: Transform::default(),
                });
            }
        }
        if let AssetKind::Video | AssetKind::Audio = asset.kind {
            self.audio.refresh(&self.project);
        }
        self.last_request = None;
    }

    fn add_text_clip(&mut self) {
        let start = self.playhead_us;
        let end = start + 3 * SECOND_US;
        self.project.text_tracks[0].clips.push(TextClip {
            text: "New title".into(),
            timeline_start: start,
            timeline_end: end,
            style: TextStyle::default(),
            transform: Transform::default(),
            opacity: 1.0,
        });
    }

    fn inspector(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("inspector")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.strong("Clip");
                ui.separator();
                match self.selected {
                    Selection::None => {
                        ui.label("Select a clip in the timeline to edit.");
                    }
                    Selection::Video { track, clip } => {
                        self.inspect_video(ui, track, clip);
                    }
                    Selection::Audio { track, clip } => {
                        self.inspect_audio(ui, track, clip);
                    }
                    Selection::Text { track, clip } => {
                        self.inspect_text(ui, track, clip);
                    }
                }
                ui.separator();
                if ui.button("+ Add text clip").clicked() {
                    self.add_text_clip();
                }
                ui.separator();
                ui.strong("Keyboard");
                ui.label("Space play/pause · ←/→ step frames");
            });
    }

    fn inspect_video(&mut self, ui: &mut egui::Ui, track: usize, clip: usize) {
        let Some(tr) = self.project.video_tracks.get(track) else { return };
        let Some(c) = tr.clips.get(clip).cloned() else { return };
        let s = crate::audio::timecode_string(c.source_out - c.source_in, self.project.fps);
        ui.label(format!("Duration: {s}"));
        let mut t = c.transform;
        ui.add(
            egui::Slider::new(&mut t.x, 0.0..=1.0)
                .text("X")
                .fixed_decimals(2),
        );
        ui.add(
            egui::Slider::new(&mut t.y, 0.0..=1.0)
                .text("Y")
                .fixed_decimals(2),
        );
        ui.add(
            egui::Slider::new(&mut t.scale, 0.1..=3.0)
                .text("Scale")
                .fixed_decimals(2),
        );
        ui.add(
            egui::Slider::new(&mut t.rotate, -180.0..=180.0)
                .text("Rotate°")
                .fixed_decimals(1),
        );
        let mut opacity = c.opacity;
        if ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0).text("Opacity")).changed() {
            if let Some(c) = self.project.video_tracks[track].clips.get_mut(clip) {
                c.opacity = opacity;
            }
        }
        if t != c.transform {
            if let Some(c) = self.project.video_tracks[track].clips.get_mut(clip) {
                c.transform = t;
            }
        }
        ui.separator();
        if ui.button("Split at playhead").clicked() {
            self.split_video(track, clip);
        }
    }

    fn inspect_audio(&mut self, ui: &mut egui::Ui, track: usize, clip: usize) {
        let Some(tr) = self.project.audio_tracks.get(track) else { return };
        let Some(c) = tr.clips.get(clip).cloned() else { return };
        let s = crate::audio::timecode_string(c.source_out - c.source_in, self.project.fps);
        ui.label(format!("Duration: {s}"));
        let mut gain = c.gain_db;
        if ui.add(egui::Slider::new(&mut gain, -60.0..=12.0).text("Gain dB")).changed() {
            if let Some(c) = self.project.audio_tracks[track].clips.get_mut(clip) {
                c.gain_db = gain;
            }
        }
        let mut fi = c.fade_in_us;
        let mut fo = c.fade_out_us;
        if ui
            .add(egui::Slider::new(&mut fi, 0..=5_000_000).text("Fade in "))
            .changed()
        {
            if let Some(c) = self.project.audio_tracks[track].clips.get_mut(clip) {
                c.fade_in_us = fi;
            }
        }
        if ui
            .add(egui::Slider::new(&mut fo, 0..=5_000_000).text("Fade out "))
            .changed()
        {
            if let Some(c) = self.project.audio_tracks[track].clips.get_mut(clip) {
                c.fade_out_us = fo;
            }
        }
        ui.separator();
        if ui.button("Split at playhead").clicked() {
            self.split_audio(track, clip);
        }
    }

    fn inspect_text(&mut self, ui: &mut egui::Ui, track: usize, clip: usize) {
        let Some(tr) = self.project.text_tracks.get(track) else { return };
        let Some(c) = tr.clips.get(clip).cloned() else { return };
        let mut text = c.text.clone();
        if ui
            .add(egui::TextEdit::multiline(&mut text).desired_rows(3))
            .changed()
        {
            if let Some(c) = self.project.text_tracks[track].clips.get_mut(clip) {
                c.text = text;
            }
        }
        let mut size = c.style.font_size;
        if ui.add(egui::Slider::new(&mut size, 8.0..=120.0).text("Size")).changed() {
            if let Some(c) = self.project.text_tracks[track].clips.get_mut(clip) {
                c.style.font_size = size;
            }
        }
        let color = c.style.color;
        let mut c32 = egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
        if ui.color_edit_button_srgba(&mut c32).changed() {
            if let Some(c) = self.project.text_tracks[track].clips.get_mut(clip) {
                c.style.color = [c32.r(), c32.g(), c32.b(), c32.a()];
            }
        }
        let mut align = c.style.align;
        egui::ComboBox::from_label("Align")
            .selected_text(format!("{align:?}"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut align, TextAlign::Left, "Left");
                ui.selectable_value(&mut align, TextAlign::Center, "Center");
                ui.selectable_value(&mut align, TextAlign::Right, "Right");
            });
        if align != c.style.align {
            if let Some(c) = self.project.text_tracks[track].clips.get_mut(clip) {
                c.style.align = align;
            }
        }
    }

    fn split_video(&mut self, track: usize, clip: usize) {
        let pt = self.playhead_us;
        let Some(c) = self.project.video_tracks[track].clips.get(clip).cloned() else {
            return;
        };
        let start = c.timeline_start;
        let end = start + (c.source_out - c.source_in);
        if pt <= start || pt >= end {
            return;
        }
        let src = c.source_in + (pt - start);
        let tr = &mut self.project.video_tracks[track];
        tr.clips[clip].source_out = src;
        tr.clips.insert(
            clip + 1,
            VideoClip {
                asset: c.asset,
                source_in: src,
                source_out: c.source_out,
                timeline_start: pt,
                opacity: c.opacity,
                transform: c.transform,
            },
        );
    }

    fn split_audio(&mut self, track: usize, clip: usize) {
        let pt = self.playhead_us;
        let Some(c) = self.project.audio_tracks[track].clips.get(clip).cloned() else {
            return;
        };
        let start = c.timeline_start;
        let end = start + (c.source_out - c.source_in);
        if pt <= start || pt >= end {
            return;
        }
        let src = c.source_in + (pt - start);
        let tr = &mut self.project.audio_tracks[track];
        tr.clips[clip].source_out = src;
        tr.clips.insert(
            clip + 1,
            AudioClip {
                asset: c.asset,
                source_in: src,
                source_out: c.source_out,
                timeline_start: pt,
                gain_db: c.gain_db,
                fade_in_us: c.fade_in_us,
                fade_out_us: c.fade_out_us,
            },
        );
        self.audio.refresh(&self.project);
    }

    fn timeline(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(200.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.strong("Timeline");
                    ui.separator();
                    if ui.small_button("+ Video/Audio").clicked() {
                        // quick append of first media asset
                        if let Some(id) = self.project.assets.assets.keys().next().copied() {
                            self.add_clip_from_media(id);
                        }
                    }
                    if ui.small_button("+ Text").clicked() {
                        self.add_text_clip();
                    }
                    ui.separator();
                    ui.checkbox(&mut self.project.video_tracks[0].muted, "V1 mute");
                    ui.checkbox(&mut self.project.audio_tracks[0].muted, "A1 mute");
                });
                ui.separator();
                let usize_dur = self.project.duration_us().max(SECOND_US);
                let w = ui.available_width().max(320.0);
                let row_h = 34.0;
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(w, row_h * 3.0 + 24.0),
                    egui::Sense::click(),
                );
                let painter = ui.painter_at(rect);

                // Time ruler
                painter.rect_filled(rect, 2.0, PANEL);
                let ruler_h = 18.0;
                let t0 = 0.0f32;
                let t1 = w;
                let step_markers = SECOND_US as f32 * (w / usize_dur.max(1) as f32).max(0.01);

                // Draw a few tick labels
                let total = w as f64 / (usize_dur as f64 / SECOND_US as f64).max(1.0);
                let _ = total;
                let _ = t0;
                let _ = t1;
                let _ = step_markers;
                let sec_px = w / (usize_dur as f32 / SECOND_US as f32).max(1.0);
                let mut s = 0;
                let mut x = 0.0;
                while x < w {
                    painter.text(
                        egui::pos2(rect.min.x + x + 2.0, rect.min.y + 2.0),
                        egui::Align2::LEFT_TOP,
                        format!("{s}s"),
                        egui::FontId::proportional(10.0),
                        WHITE,
                    );
                    s += 1;
                    x += sec_px;
                }

                // Track lanes.
                let mut clicks: Vec<(usize, usize, u8)> = Vec::new();
                for (ti, tr) in self.project.clone().video_tracks.iter().enumerate() {
                    let y0 = rect.min.y + ruler_h + ti as f32 * row_h;
                    let y1 = y0 + row_h - 3.0;
                    let lane = egui::Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(rect.max.x, y1),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.text(
                        lane.min + egui::vec2(4.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        tr.name.clone(),
                        egui::FontId::proportional(11.0),
                        WHITE,
                    );
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw = ((c.source_out - c.source_in) as f32 / usize_dur as f32 * w).max(6.0);
                        let cx = (c.timeline_start as f32 / usize_dur as f32 * w).max(0.0);
                        let cr = egui::Rect::from_min_max(
                            egui::pos2(rect.min.x + cx + 2.0, y0 + 2.0),
                            egui::pos2(
                                rect.min.x + cx + cw - 2.0,
                                y1 - 2.0,
                            ),
                        );
                        painter.rect_filled(cr, 2.0, VIDEO_CLIP);
                        painter.text(
                            cr.min + egui::vec2(4.0, 4.0),
                            egui::Align2::LEFT_TOP,
                            &c_name(&self.project, c.asset),
                            egui::FontId::proportional(10.0),
                            WHITE,
                        );
                        // click region
                        if response.hover_pos().is_some_and(|p| cr.contains(p)) && response.clicked() {
                            clicks.push((ti, ci, 1));
                        }
                    }
                }
                // audio tracks second block (different rows)
                let vid_rows = self.project.video_tracks.len();
                for (ai, tr) in self.project.clone().audio_tracks.iter().enumerate() {
                    let y0 = rect.min.y + ruler_h + (vid_rows + ai) as f32 * row_h;
                    let y1 = y0 + row_h - 3.0;
                    let lane = egui::Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(rect.max.x, y1),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.text(
                        lane.min + egui::vec2(4.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        tr.name.clone(),
                        egui::FontId::proportional(11.0),
                        WHITE,
                    );
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw = ((c.source_out - c.source_in) as f32 / usize_dur as f32 * w).max(6.0);
                        let cx = (c.timeline_start as f32 / usize_dur as f32 * w).max(0.0);
                        let cr = egui::Rect::from_min_max(
                            egui::pos2(rect.min.x + cx + 2.0, y0 + 4.0),
                            egui::pos2(rect.min.x + cx + cw - 2.0, y1 - 4.0),
                        );
                        painter.rect_filled(cr, 2.0, AUDIO_CLIP);
                        if let Some(asset) = self.project.assets.get(c.asset) {
                            if self.show_peaks && !asset.peaks.is_empty() {
                                let pw = cr.width();
                                let ph = cr.height();
                                let n = asset.peaks.len();
                                let step = (n as f32 / pw).ceil().max(1.0) as usize;
                                let mut i = 0usize;
                                let mut px = 0.0f32;
                                while px < pw {
                                    let mut max = 0.0f32;
                                    let mut min = 0.0f32;
                                    for _ in 0..step {
                                        if i < n {
                                            let (a, b) = asset.peaks[i];
                                            max = max.max(b);
                                            min = min.min(a);
                                        }
                                        i += 1;
                                    }
                                    let cxm = cr.min.x + px;
                                    let mid = cr.center().y;
                                    painter.line_segment(
                                        [
                                            egui::pos2(cxm, mid - max * ph * 0.5),
                                            egui::pos2(cxm, mid - min * ph * 0.5),
                                        ],
                                        egui::Stroke::new(1.0, WAVE),
                                    );
                                    px += 1.0;
                                }
                            }
                        }
                        if response.hover_pos().is_some_and(|p| cr.contains(p)) && response.clicked() {
                            clicks.push((ai, ci, 2));
                        }
                    }
                }
                // text tracks
                let base_rows = vid_rows + self.project.audio_tracks.len();
                for (ti, tr) in self.project.clone().text_tracks.iter().enumerate() {
                    let y0 = rect.min.y + ruler_h + (base_rows + ti) as f32 * row_h;
                    let y1 = y0 + row_h - 3.0;
                    let lane = egui::Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(rect.max.x, y1),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.text(
                        lane.min + egui::vec2(4.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        tr.name.clone(),
                        egui::FontId::proportional(11.0),
                        WHITE,
                    );
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw = ((c.timeline_end - c.timeline_start) as f32 / usize_dur as f32 * w)
                            .max(6.0);
                        let cx = (c.timeline_start as f32 / usize_dur as f32 * w).max(0.0);
                        let cr = egui::Rect::from_min_max(
                            egui::pos2(rect.min.x + cx + 2.0, y0 + 4.0),
                            egui::pos2(rect.min.x + cx + cw - 2.0, y1 - 4.0),
                        );
                        painter.rect_filled(cr, 2.0, TEXT_CLIP);
                        if response.hover_pos().is_some_and(|p| cr.contains(p)) && response.clicked() {
                            clicks.push((ti, ci, 3));
                        }
                    }
                }

                if let Some((ti, ci, kind)) = clicks.last().copied() {
                    self.selected = match kind {
                        1 => Selection::Video { track: ti, clip: ci },
                        2 => Selection::Audio { track: ti, clip: ci },
                        _ => Selection::Text { track: ti, clip: ci },
                    };
                }

                // Playhead line
                let px = rect.min.x
                    + (self.playhead_us as f32 / usize_dur as f32 * w).clamp(0.0, w);
                painter.line_segment(
                    [
                        egui::pos2(px, rect.min.y),
                        egui::pos2(px, rect.max.y),
                    ],
                    egui::Stroke::new(2.0, PLAYHEAD),
                );

                // Click on ruler seeks.
                if response.clicked() && response.hover_pos().is_some() {
                    let p = response.hover_pos().unwrap();
                    if p.y < rect.min.y + ruler_h {
                        let frac = ((p.x - rect.min.x) / w).clamp(0.0, 1.0);
                        self.playhead_us = (usize_dur as f64 * frac as f64) as i64;
                        self.seek_marker();
                    }
                }
            });
    }

    fn export_window(&mut self, ctx: &egui::Context) {
        let mut open = self.export_open;
        egui::Window::new("Export")
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label("Render the timeline to MP4 (H.264 + AAC).");
                ui.horizontal(|ui| {
                    ui.label("File:");
                    ui.text_edit_singleline(&mut self.export_path);
                    if ui.button("Browse").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("MP4", &["mp4"])
                            .set_file_name("out.mp4")
                            .save_file()
                        {
                            self.export_path = p.to_string_lossy().into_owned();
                        }
                    }
                });
                egui::Grid::new("export_grid").num_columns(2).show(ui, |inner| {
                    inner.label("Width");
                    inner.add(egui::DragValue::new(&mut self.export_w).range(320..=3840));
                    inner.end_row();
                    inner.label("Height");
                    inner.add(egui::DragValue::new(&mut self.export_h).range(240..=2160));
                    inner.end_row();
                    inner.label("FPS");
                    inner.add(egui::DragValue::new(&mut self.export_fps).range(10.0..=60.0));
                    inner.end_row();
                });
                if ui
                    .add_sized([120.0, 30.0], egui::Button::new("Render").fill(ACCENT))
                    .on_hover_text("Renders the full timeline (blocking, can take a while)")
                    .clicked()
                {
                    let project = self.project.clone();
                    let cfg = crate::export::ExportConfig {
                        path: self.export_path.clone(),
                        width: self.export_w,
                        height: self.export_h,
                        fps: self.export_fps,
                        bitrate_kbps: 8000,
                    };
                    let result = std::thread::spawn(move || {
                        crate::export::export_project(&project, &cfg, &mut |_p| {})
                    })
                    .join();
                    match result {
                        Ok(Ok(())) => self.export_msg = Some("Export complete.".into()),
                        Ok(Err(e)) => self.export_msg = Some(format!("Export failed: {e:#}")),
                        Err(_) => self.export_msg = Some("Export thread panicked.".into()),
                    }
                }
                if let Some(msg) = &self.export_msg {
                    ui.separator();
                    ui.label(msg);
                }
            });
        self.export_open = open;
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if !self.toasts.is_empty() {
                    let t = self.toasts.remove(0);
                    ui.colored_label(Color32::from_rgb(120, 220, 160), t);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "{}×{} @{}fps   {} assets",
                        self.project.width,
                        self.project.height,
                        self.project.fps,
                        self.project.assets.len()
                    ));
                });
            });
        });
    }
}

fn c_name(p: &Project, id: AssetId) -> String {
    p.assets
        .get(id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "?".into())
}

const WHITE: Color32 = Color32::from_rgb(236, 236, 240);
const PANEL: Color32 = Color32::from_rgb(12, 12, 14);
const TRACK: Color32 = Color32::from_rgb(18, 18, 22);
const BUTTON: Color32 = Color32::from_rgb(32, 32, 38);
const ACCENT: Color32 = Color32::from_rgb(60, 120, 210);
const VIDEO_CLIP: Color32 = Color32::from_rgb(40, 70, 120);
const AUDIO_CLIP: Color32 = Color32::from_rgb(46, 108, 74);
const TEXT_CLIP: Color32 = Color32::from_rgb(120, 90, 150);
const WAVE: Color32 = Color32::from_rgb(120, 220, 170);
const PLAYHEAD: Color32 = Color32::from_rgb(255, 80, 80);

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = PANEL;
    style.visuals.window_fill = PANEL;
    style.visuals.extreme_bg_color = TRACK;
    style.visuals.faint_bg_color = Color32::from_rgb(10, 10, 12);
    style.visuals.override_text_color = Some(WHITE);
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, WHITE);
    style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, WHITE);
    style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, WHITE);
    style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5, WHITE);
    ctx.set_style(style);
}