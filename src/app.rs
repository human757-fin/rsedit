//! Fast Cutter — the editor application itself.
//!
//! Layout: top menu bar + transport bar, left media panel (tabbed, with
//! drag-and-drop), center preview viewport, right clip inspector, bottom
//! multi-track timeline. Theme is near-black (#08080a) with near-white
//! foreground. Preferences (editable keybinds + global settings) are under
//! File -> Preferences and Help -> Preferences.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, Key, KeyboardShortcut, Modifiers, Rect, RichText, Sense, StrokeKind};

use crate::audio::{timecode_string, AudioEngine};
use crate::decoder::ensure_ffmpeg;
use crate::framecache::{FrameCache, FrameRequest};
use crate::timeline::{
    AssetId, AssetKind, AudioClip, Project, TextAlign, TextClip, TextStyle, Timecode, Track,
    Transform, VideoClip, SECOND_US,
};

// ── Keyboard shortcuts ───────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Keybinds {
    play_pause: KeyboardShortcut,
    step_fwd: KeyboardShortcut,
    step_back: KeyboardShortcut,
    jump_start: KeyboardShortcut,
    jump_end: KeyboardShortcut,
    split_clip: KeyboardShortcut,
    delete_clip: KeyboardShortcut,
    undo: KeyboardShortcut,
    redo: KeyboardShortcut,
    export_render: KeyboardShortcut,
    add_text: KeyboardShortcut,
    toggle_peaks: KeyboardShortcut,
    add_video_track: KeyboardShortcut,
    add_audio_track: KeyboardShortcut,
    close_window: KeyboardShortcut,
}

impl Default for Keybinds {
    fn default() -> Self {
        Self {
            play_pause: KeyboardShortcut::new(Modifiers::NONE, Key::Space),
            step_fwd: KeyboardShortcut::new(Modifiers::NONE, Key::ArrowRight),
            step_back: KeyboardShortcut::new(Modifiers::NONE, Key::ArrowLeft),
            jump_start: KeyboardShortcut::new(Modifiers::NONE, Key::Home),
            jump_end: KeyboardShortcut::new(Modifiers::NONE, Key::End),
            split_clip: KeyboardShortcut::new(Modifiers::CTRL, Key::S),
            delete_clip: KeyboardShortcut::new(Modifiers::NONE, Key::Delete),
            undo: KeyboardShortcut::new(Modifiers::CTRL, Key::Z),
            redo: KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Z),
            export_render: KeyboardShortcut::new(Modifiers::CTRL, Key::E),
            add_text: KeyboardShortcut::new(Modifiers::CTRL, Key::T),
            toggle_peaks: KeyboardShortcut::new(Modifiers::NONE, Key::P),
            add_video_track: KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::V),
            add_audio_track: KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::A),
            close_window: KeyboardShortcut::new(Modifiers::NONE, Key::Escape),
        }
    }
}

impl Keybinds {
    fn get(&self, f: BindField) -> KeyboardShortcut {
        match f {
            BindField::PlayPause => self.play_pause,
            BindField::StepBack => self.step_back,
            BindField::StepFwd => self.step_fwd,
            BindField::JumpStart => self.jump_start,
            BindField::JumpEnd => self.jump_end,
            BindField::SplitClip => self.split_clip,
            BindField::DeleteClip => self.delete_clip,
            BindField::Undo => self.undo,
            BindField::Redo => self.redo,
            BindField::ExportRender => self.export_render,
            BindField::AddText => self.add_text,
            BindField::TogglePeaks => self.toggle_peaks,
            BindField::AddVideoTrack => self.add_video_track,
            BindField::AddAudioTrack => self.add_audio_track,
            BindField::CloseWindow => self.close_window,
        }
    }

    fn set(&mut self, f: BindField, sc: KeyboardShortcut) {
        match f {
            BindField::PlayPause => self.play_pause = sc,
            BindField::StepBack => self.step_back = sc,
            BindField::StepFwd => self.step_fwd = sc,
            BindField::JumpStart => self.jump_start = sc,
            BindField::JumpEnd => self.jump_end = sc,
            BindField::SplitClip => self.split_clip = sc,
            BindField::DeleteClip => self.delete_clip = sc,
            BindField::Undo => self.undo = sc,
            BindField::Redo => self.redo = sc,
            BindField::ExportRender => self.export_render = sc,
            BindField::AddText => self.add_text = sc,
            BindField::TogglePeaks => self.toggle_peaks = sc,
            BindField::AddVideoTrack => self.add_video_track = sc,
            BindField::AddAudioTrack => self.add_audio_track = sc,
            BindField::CloseWindow => self.close_window = sc,
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
enum BindField {
    PlayPause,
    StepBack,
    StepFwd,
    JumpStart,
    JumpEnd,
    SplitClip,
    DeleteClip,
    Undo,
    Redo,
    ExportRender,
    AddText,
    TogglePeaks,
    AddVideoTrack,
    AddAudioTrack,
    CloseWindow,
}

// ── Global application settings ──────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Settings {
    show_peaks: bool,
    dark_mode: bool,
    snap_to_playhead: bool,
    preview_width: u32,
    preview_height: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_peaks: true,
            dark_mode: true,
            snap_to_playhead: true,
            preview_width: 1280,
            preview_height: 720,
        }
    }
}

const BIND_ROWS: [(&str, BindField); 15] = [
    ("Play / Pause", BindField::PlayPause),
    ("Step one frame back", BindField::StepBack),
    ("Step one frame forward", BindField::StepFwd),
    ("Jump to start", BindField::JumpStart),
    ("Jump to end", BindField::JumpEnd),
    ("Split clip at playhead", BindField::SplitClip),
    ("Delete selected clip", BindField::DeleteClip),
    ("Undo", BindField::Undo),
    ("Redo", BindField::Redo),
    ("Open / close export", BindField::ExportRender),
    ("Add text clip", BindField::AddText),
    ("Toggle audio peaks", BindField::TogglePeaks),
    ("Add video track", BindField::AddVideoTrack),
    ("Add audio track", BindField::AddAudioTrack),
    ("Close dialogs", BindField::CloseWindow),
];

fn bind_label(f: BindField) -> &'static str {
    for (label, bf) in BIND_ROWS {
        if bf == f {
            return label;
        }
    }
    "?"
}

// ── Media panel tab ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaTab {
    All,
    Video,
    Audio,
}

// ── Application state ────────────────────────────────────────────────────────

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
    // UI state
    media_tab: MediaTab,
    media_filter: String,
    thumbnails: HashMap<AssetId, egui::TextureHandle>,
    drag_source: Option<AssetId>,
    drag_timeline_us: Option<i64>,
    settings_open: bool,
    capturing: Option<BindField>,
    keybinds: Keybinds,
    settings: Settings,
    // Export dialog
    export_open: bool,
    export_path: String,
    export_w: u32,
    export_h: u32,
    export_fps: f64,
    export_msg: Option<String>,
    toasts: Vec<(String, Instant)>,
    // Undo / redo stacks
    undo_stack: Vec<Project>,
    redo_stack: Vec<Project>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        apply_theme(&cc.egui_ctx, true);
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
            media_tab: MediaTab::All,
            media_filter: String::new(),
            thumbnails: HashMap::new(),
            drag_source: None,
            drag_timeline_us: None,
            settings_open: false,
            capturing: None,
            keybinds: Keybinds::default(),
            settings: Settings::default(),
            export_open: false,
            export_path: "out.mp4".into(),
            export_w: 1920,
            export_h: 1080,
            export_fps: 30.0,
            export_msg: None,
            toasts: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.project.clone());
        self.redo_stack.clear();
        if self.undo_stack.len() > 100 {
            self.undo_stack.remove(0);
        }
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.project.clone());
            self.project = prev;
            self.audio.refresh(&self.project);
            self.last_request = None;
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.project.clone());
            self.project = next;
            self.audio.refresh(&self.project);
            self.last_request = None;
        }
    }

    fn compute_preview(&mut self) {
        let dur = self.project.duration_us().max(SECOND_US);
        let pts = self.playhead_us.min(dur);
        let w = self.settings.preview_width;
        let h = self.settings.preview_height;
        if let Some(tex) = self.preview_texture.as_mut() {
            let rgba = crate::render::Composition {
                project: &self.project,
                cache: &self.cache,
                w,
                h,
            }
            .render_at(pts);
            let img =
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            tex.set(img, egui::TextureOptions::LINEAR);
        }
        self.preview_rendered_pts = Some(pts);
    }

    fn request_frame(&mut self) {
        let pts = self.playhead_us;
        if let Some((_ti, clip)) = top_video_clip(&self.project, pts) {
            if let Some(asset) = self.project.assets.get(clip.asset) {
                if asset.kind == AssetKind::Image {
                    return;
                }
                let key = (asset.id.0, pts);
                if self.last_request != Some(key) {
                    self.cache.set_target(FrameRequest {
                        asset: asset.id.0,
                        path: asset.path.clone(),
                        w: self.settings.preview_width,
                        h: self.settings.preview_height,
                        pts_us: pts,
                    });
                    self.last_request = Some(key);
                }
            }
        }
    }

    fn step_frame(&mut self, delta: i64) {
        let us_per_frame = (SECOND_US as f64 / self.project.fps) as i64;
        self.playhead_us = (self.playhead_us + delta * us_per_frame)
            .max(0)
            .min(self.project.duration_us().max(SECOND_US));
        self.seek_marker();
    }

    fn delete_selected(&mut self) {
        let sel = match self.selected {
            Selection::None => return,
            other => other,
        };
        let (kind, track, clip) = match sel {
            Selection::Video { track, clip } => (0u8, track, clip),
            Selection::Audio { track, clip } => (1u8, track, clip),
            Selection::Text { track, clip } => (2u8, track, clip),
            Selection::None => return,
        };
        let len = match kind {
            0 => self.project.video_tracks.get(track).map(|t| t.clips.len()),
            1 => self.project.audio_tracks.get(track).map(|t| t.clips.len()),
            _ => self.project.text_tracks.get(track).map(|t| t.clips.len()),
        };
        if let Some(len) = len {
            if clip < len {
                self.push_undo();
                if kind == 0 {
                    self.project.video_tracks[track].clips.remove(clip);
                } else if kind == 1 {
                    self.project.audio_tracks[track].clips.remove(clip);
                } else {
                    self.project.text_tracks[track].clips.remove(clip);
                }
                self.selected = Selection::None;
            }
        }
        self.audio.refresh(&self.project);
        self.last_request = None;
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

    fn add_track(&mut self, kind: u8) {
        self.push_undo();
        match kind {
            0 => {
                let n = self.project.video_tracks.len() + 1;
                self.project.video_tracks.push(Track {
                    name: format!("V{n}"),
                    ..Default::default()
                });
            }
            1 => {
                let n = self.project.audio_tracks.len() + 1;
                self.project.audio_tracks.push(Track {
                    name: format!("A{n}"),
                    ..Default::default()
                });
            }
            _ => {}
        }
        self.last_request = None;
    }

    fn handle_keybinds(&mut self, ctx: &egui::Context) {
        if self.capturing.is_some() {
            return;
        }
        let kb = self.keybinds;
        ctx.input_mut(|i| {
            // Most-specific shortcuts first (redo before undo).
            if i.consume_shortcut(&kb.redo) {
                self.redo();
            } else if i.consume_shortcut(&kb.undo) {
                self.undo();
            } else if i.consume_shortcut(&kb.delete_clip) {
                self.delete_selected();
            } else if i.consume_shortcut(&kb.split_clip) {
                match self.selected {
                    Selection::Video { track, clip } => self.split_video(track, clip),
                    Selection::Audio { track, clip } => self.split_audio(track, clip),
                    _ => {}
                }
            } else if i.consume_shortcut(&kb.play_pause) {
                self.toggle_play();
            } else if i.consume_shortcut(&kb.step_fwd) {
                self.step_frame(1);
            } else if i.consume_shortcut(&kb.step_back) {
                self.step_frame(-1);
            } else if i.consume_shortcut(&kb.jump_start) {
                self.playhead_us = 0;
                self.seek_marker();
            } else if i.consume_shortcut(&kb.jump_end) {
                self.playhead_us = self.project.duration_us().max(SECOND_US);
                self.seek_marker();
            } else if i.consume_shortcut(&kb.export_render) {
                self.export_open = !self.export_open;
            } else if i.consume_shortcut(&kb.add_text) {
                self.add_text_clip();
            } else if i.consume_shortcut(&kb.toggle_peaks) {
                self.settings.show_peaks = !self.settings.show_peaks;
            } else if i.consume_shortcut(&kb.add_video_track) {
                self.add_track(0);
            } else if i.consume_shortcut(&kb.add_audio_track) {
                self.add_track(1);
            } else if i.consume_shortcut(&kb.close_window) {
                self.export_open = false;
                self.settings_open = false;
            }
        });
    }

    fn handle_capture(&mut self, ctx: &egui::Context) {
        let Some(field) = self.capturing else {
            return;
        };
        let (combo, cancel) = ctx.input(|i| {
            let mut combo: Option<KeyboardShortcut> = None;
            let mut cancel = false;
            for ev in &i.events {
                if let egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } = ev
                {
                    if *pressed {
                        if *key == Key::Escape {
                            cancel = true;
                        } else {
                            combo = Some(KeyboardShortcut::new(*modifiers, *key));
                        }
                    }
                }
            }
            (combo, cancel)
        });
        if let Some(sc) = combo {
            self.keybinds.set(field, sc);
            self.capturing = None;
            self.toasts.push((
                format!("Bound {} to {}", bind_label(field), shortcut_label(sc)),
                Instant::now(),
            ));
        } else if cancel {
            self.capturing = None;
        }
    }
}

fn top_video_clip(p: &Project, t: Timecode) -> Option<(usize, &VideoClip)> {
    p.video_tracks.iter().enumerate().rev().find_map(|(ti, tr)| {
        tr.clips
            .iter()
            .find(|c| {
                c.timeline_start <= t && t < c.timeline_start + (c.source_out - c.source_in)
            })
            .map(|c| (ti, c))
    })
}

// ──────────────────────────────────────────────────────────────────────────────
//  eframe app loop
// ──────────────────────────────────────────────────────────────────────────────

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

        // Keyboard shortcuts.
        self.handle_keybinds(ctx);
        // Rebind capture (only active while the Preferences window is capturing).
        self.handle_capture(ctx);

        // Files dropped onto the window (from the OS file manager).
        let dropped: Vec<PathBuf> = ctx
            .input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            self.import_media(p);
        }

        self.request_frame();
        if self.preview_texture.is_none() {
            let size = [
                self.settings.preview_width as usize,
                self.settings.preview_height as usize,
            ];
            let s = ctx.load_texture(
                "preview",
                egui::ColorImage::new(size, Color32::from_rgb(8, 8, 10)),
                egui::TextureOptions::LINEAR,
            );
            self.preview_texture = Some(s);
        }
        if self.preview_rendered_pts
            != Some(self.playhead_us.min(self.project.duration_us().max(SECOND_US)))
        {
            self.compute_preview();
        }

        // Panels. Timeline must be added first so it spans the whole bottom
        // width and the side panels stop at its top edge.
        self.menu_bar(ctx);
        self.transport_bar(ctx);
        self.timeline(ctx);
        self.media_panel(ctx);
        self.inspector(ctx);
        self.preview_panel(ctx);
        if self.export_open {
            self.export_window(ctx);
        }
        if self.settings_open {
            self.settings_window(ctx);
        }
        self.toast_overlay(ctx);

        // Clear a pending media drag when the pointer was released.
        if ctx.input(|i| i.pointer.any_released()) {
            self.drag_source = None;
        }
        ctx.request_repaint();
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Panels
// ──────────────────────────────────────────────────────────────────────────────

impl FastCutterApp {
    // ── Menu bar ─────────────────────────────────────────────────────────────
    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui
                        .add(egui::Button::new("Import media..."))
                        .clicked()
                    {
                        ui.close_menu();
                        self.pick_and_import();
                    }
                    if ui.add(egui::Button::new("Export...").shortcut_text("Ctrl+E")).clicked() {
                        ui.close_menu();
                        self.export_open = true;
                    }
                    ui.separator();
                    if ui
                        .add(egui::Button::new("Preferences..."))
                        .clicked()
                    {
                        ui.close_menu();
                        self.settings_open = true;
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.add(egui::Button::new("Undo").shortcut_text("Ctrl+Z")).clicked() {
                        ui.close_menu();
                        self.undo();
                    }
                    if ui
                        .add(egui::Button::new("Redo").shortcut_text("Ctrl+Shift+Z"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.redo();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Delete clip").shortcut_text("Delete")).clicked() {
                        ui.close_menu();
                        self.delete_selected();
                    }
                    if ui
                        .add(egui::Button::new("Split at playhead").shortcut_text("Ctrl+S"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.split_selected();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("Add text clip").shortcut_text("Ctrl+T")).clicked() {
                        ui.close_menu();
                        self.add_text_clip();
                    }
                    if ui
                        .add(egui::Button::new("Add video track").shortcut_text("Ctrl+Shift+V"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.add_track(0);
                    }
                    if ui
                        .add(egui::Button::new("Add audio track").shortcut_text("Ctrl+Shift+A"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.add_track(1);
                    }
                    ui.separator();
                    if ui
                        .add(egui::Button::new("Toggle audio peaks").shortcut_text("P"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.settings.show_peaks = !self.settings.show_peaks;
                    }
                });
            });
            ui.add_space(2.0);
        });
    }

    // ── Transport bar ────────────────────────────────────────────────────────
    fn transport_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("transport").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let play_label = if self.playing { "Stop" } else { "Play" };
                if ui
                    .add_sized([44.0, 26.0], egui::Button::new(play_label).fill(BUTTON))
                    .on_hover_text(shortcut_label(self.keybinds.play_pause))
                    .clicked()
                {
                    self.toggle_play();
                }
                if ui
                    .add_sized([26.0, 26.0], egui::Button::new("<|").fill(BUTTON))
                    .on_hover_text(shortcut_label(self.keybinds.jump_start))
                    .clicked()
                {
                    self.playhead_us = 0;
                    self.seek_marker();
                }
                if ui
                    .add_sized([26.0, 26.0], egui::Button::new("<").fill(BUTTON))
                    .on_hover_text(shortcut_label(self.keybinds.step_back))
                    .clicked()
                {
                    self.step_frame(-1);
                }
                if ui
                    .add_sized([26.0, 26.0], egui::Button::new(">").fill(BUTTON))
                    .on_hover_text(shortcut_label(self.keybinds.step_fwd))
                    .clicked()
                {
                    self.step_frame(1);
                }
                if ui
                    .add_sized([26.0, 26.0], egui::Button::new(">|").fill(BUTTON))
                    .on_hover_text(shortcut_label(self.keybinds.jump_end))
                    .clicked()
                {
                    self.playhead_us = self.project.duration_us().max(SECOND_US);
                    self.seek_marker();
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{} / {}",
                        timecode_string(self.playhead_us, self.project.fps),
                        timecode_string(self.project.duration_us(), self.project.fps)
                    ))
                    .monospace()
                    .color(WHITE),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                if ui
                    .add_sized(
                        [60.0, 26.0],
                        egui::Button::new("Export").fill(ACCENT),
                    )
                    .on_hover_text(shortcut_label(self.keybinds.export_render))
                    .clicked()
                {
                    self.export_open = true;
                }
            });
            ui.add_space(2.0);
        });
    }

    // ── Media panel (left, tabbed, drag-drop) ────────────────────────────────
    fn media_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("media")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.strong("Media");
                ui.add_space(4.0);

                // Tab bar.
                ui.horizontal(|ui| {
                    for (label, tab) in [
                        ("All", MediaTab::All),
                        ("Video", MediaTab::Video),
                        ("Audio", MediaTab::Audio),
                    ] {
                        let selected = self.media_tab == tab;
                        let btn = if selected {
                            egui::Button::new(RichText::new(label).color(ACCENT))
                                .fill(BUTTON)
                                .stroke(egui::Stroke::new(1.0, ACCENT))
                        } else {
                            egui::Button::new(RichText::new(label).color(WHITE)).fill(BUTTON)
                        };
                        if ui.add_sized([58.0, 22.0], btn).clicked() {
                            self.media_tab = tab;
                        }
                    }
                });

                ui.add_space(4.0);

                if ui
                    .add_sized(
                        [ui.available_width(), 26.0],
                        egui::Button::new(RichText::new("+ Import").color(WHITE)).fill(ACCENT),
                    )
                    .clicked()
                {
                    self.pick_and_import();
                }

                ui.add_space(4.0);
                ui.text_edit_singleline(&mut self.media_filter);
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Drag clips onto the timeline, or double-click to add.")
                        .small()
                        .color(Color32::from_rgb(140, 140, 150)),
                );
                ui.add_space(4.0);

                let mut add_id: Option<AssetId> = None;
                let mut remove_id: Option<AssetId> = None;
                let ids: Vec<AssetId> = self.project.assets.assets.keys().copied().collect();
                let fps = self.project.fps;
                let query = self.media_filter.trim().to_lowercase();

                // Lazily upload thumbnails for any asset that has one cached.
                {
                    let ctx = ui.ctx().clone();
                    let assets = &self.project.assets;
                    for id in &ids {
                        if self.thumbnails.contains_key(id) {
                            continue;
                        }
                        if let Some((w, h, rgba)) =
                            assets.get(*id).and_then(|a| a.thumb.clone())
                        {
                            let img = egui::ColorImage::from_rgba_unmultiplied(
                                [w as usize, h as usize],
                                &rgba,
                            );
                            let tex = ctx
                                .load_texture(format!("thumb_{}", id.0), img, egui::TextureOptions::LINEAR);
                            self.thumbnails.insert(*id, tex);
                        }
                    }
                }

                egui::ScrollArea::vertical()
                    .max_height(ui.available_height())
                    .show(ui, |ui| {
                        for id in ids {
                            let Some(a) = self.project.assets.get(id) else {
                                continue;
                            };
                            let visible = match self.media_tab {
                                MediaTab::All => true,
                                MediaTab::Video => {
                                    matches!(a.kind, AssetKind::Video | AssetKind::Image)
                                }
                                MediaTab::Audio => a.kind == AssetKind::Audio,
                            };
                            if !visible {
                                continue;
                            }
                            if !query.is_empty() && !a.name.to_lowercase().contains(&query) {
                                continue;
                            }

                            let (badge, bg) = match a.kind {
                                AssetKind::Video => ("V", VIDEO_CLIP),
                                AssetKind::Audio => ("A", AUDIO_CLIP),
                                AssetKind::Image => ("I", TEXT_CLIP),
                            };

                            let item_response = ui.allocate_response(
                                egui::vec2(ui.available_width(), 42.0),
                                Sense::click_and_drag(),
                            );

                            // Background.
                            let bg_col = if item_response.hovered() || item_response.dragged() {
                                TRACK
                            } else {
                                PANEL
                            };
                            ui.painter()
                                .rect_filled(item_response.rect, 3.0, bg_col);

                            // Thumbnail or badge on the left.
                            let thumb_rect = Rect::from_min_size(
                                item_response.rect.min + egui::vec2(3.0, 5.0),
                                egui::vec2(56.0, 32.0),
                            );
                            if let Some(tex) = self.thumbnails.get(&id) {
                                ui.painter().rect_filled(
                                    thumb_rect,
                                    2.0,
                                    Color32::from_rgb(0, 0, 0),
                                );
                                ui.painter().image(
                                    tex.id(),
                                    thumb_rect.shrink(1.0),
                                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            } else {
                                ui.painter().rect_filled(thumb_rect, 3.0, bg);
                                ui.painter().text(
                                    thumb_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    badge,
                                    egui::FontId::proportional(11.0),
                                    Color32::from_rgb(230, 230, 235),
                                );
                            }

                            // Name + duration (right of thumbnail).
                            ui.painter().text(
                                item_response.rect.min + egui::vec2(68.0, 6.0),
                                egui::Align2::LEFT_TOP,
                                &a.name,
                                egui::FontId::proportional(11.0),
                                WHITE,
                            );
                            ui.painter().text(
                                item_response.rect.min + egui::vec2(68.0, 22.0),
                                egui::Align2::LEFT_TOP,
                                timecode_string(a.duration_us, fps),
                                egui::FontId::proportional(9.0),
                                Color32::from_rgb(150, 150, 160),
                            );

                            // Mini waveform preview for audio (right side).
                            if a.kind == AssetKind::Audio && !a.peaks.is_empty() {
                                let wf = Rect::from_min_max(
                                    egui::pos2(item_response.rect.right() - 130.0, item_response.rect.min.y + 8.0),
                                    egui::pos2(item_response.rect.right() - 6.0, item_response.rect.max.y - 8.0),
                                );
                                ui.painter().rect_filled(wf, 2.0, TRACK);
                                let mid = wf.center().y;
                                let half = wf.height() * 0.5;
                                let n = a.peaks.len();
                                let step = (n as f32 / wf.width()).ceil().max(1.0) as usize;
                                let mut i = 0usize;
                                let mut px = wf.min.x;
                                while px < wf.max.x {
                                    let mut max = 0.0f32;
                                    let mut min = 0.0f32;
                                    for _ in 0..step {
                                        if i < n {
                                            let (lo, hi) = a.peaks[i];
                                            max = max.max(hi);
                                            min = min.min(lo);
                                        }
                                        i += 1;
                                    }
                                    ui.painter().line_segment(
                                        [egui::pos2(px, mid - max * half), egui::pos2(px, mid - min * half)],
                                        egui::Stroke::new(1.0, WAVE),
                                    );
                                    px += 1.0;
                                }
                            }

                            // Drag from media -> timeline.
                            if item_response.dragged() {
                                self.drag_source = Some(id);
                                ui.painter().rect_stroke(
                                    item_response.rect,
                                    3.0,
                                    egui::Stroke::new(1.0, ACCENT),
                                    StrokeKind::Inside,
                                );
                            }
                            // Double-click adds to the end of the timeline.
                            if item_response.double_clicked() {
                                add_id = Some(id);
                            }
                            // Right-click removes the asset.
                            if item_response.secondary_clicked() {
                                remove_id = Some(id);
                            }
                        }
                    });

                ui.add_space(2.0);
                ui.separator();
                ui.checkbox(&mut self.settings.show_peaks, "Show audio peaks");

                if let Some(id) = add_id {
                    self.push_undo();
                    self.add_clip_from_media(id);
                }
                if let Some(id) = remove_id {
                    self.push_undo();
                    self.remove_asset(id);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{} assets", self.project.assets.len()));
                });
            });
    }

    // ── Preview viewport (center) ────────────────────────────────────────────
    fn preview_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(tex) = &self.preview_texture {
                let avail = ui.available_size();
                let aspect = self.settings.preview_width as f32 / self.settings.preview_height as f32;
                let mut size = avail;
                if size.x / size.y > aspect {
                    size.x = size.y * aspect;
                } else {
                    size.y = size.x / aspect;
                }
                if size.x < 8.0 || size.y < 8.0 {
                    return;
                }
                let center = ui.available_rect_before_wrap().center();
                let img_rect = Rect::from_center_size(center, size);
                ui.painter()
                    .rect_stroke(
                        img_rect.shrink(1.0),
                        2.0,
                        egui::Stroke::new(1.0, TRACK),
                        StrokeKind::Inside,
                    );
                let sized = egui::load::SizedTexture::new(tex.id(), size);
                ui.put(
                    img_rect,
                    egui::Image::new(sized).fit_to_exact_size(size),
                );
            }
        });
    }

    // ── Timeline (bottom) ────────────────────────────────────────────────────
    fn timeline(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(220.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.strong("Timeline");
                    ui.separator();
                    if ui.small_button("+ Text").clicked() {
                        self.add_text_clip();
                    }
                    if ui.small_button("+ Video track").clicked() {
                        self.add_track(0);
                    }
                    if ui.small_button("+ Audio track").clicked() {
                        self.add_track(1);
                    }
                    ui.separator();
                    for i in 0..self.project.video_tracks.len() {
                        let name = self.project.video_tracks[i].name.clone();
                        let mut muted = self.project.video_tracks[i].muted;
                        if ui.checkbox(&mut muted, &name).changed() {
                            self.project.video_tracks[i].muted = muted;
                        }
                    }
                    for i in 0..self.project.audio_tracks.len() {
                        let name = self.project.audio_tracks[i].name.clone();
                        let mut muted = self.project.audio_tracks[i].muted;
                        if ui.checkbox(&mut muted, &name).changed() {
                            self.project.audio_tracks[i].muted = muted;
                            self.audio.refresh(&self.project);
                        }
                    }
                });
                ui.separator();

                let usize_dur = self.project.duration_us().max(SECOND_US);
                let w = ui.available_width().max(320.0);
                let row_h = 34.0;
                let ruler_h = 18.0;
                let vid_rows = self.project.video_tracks.len().max(1);
                let aud_rows = self.project.audio_tracks.len().max(1);
                let txt_rows = self.project.text_tracks.len().max(1);
                let total_rows = (vid_rows + aud_rows + txt_rows) as f32;
                let total_h = ruler_h + total_rows * row_h;

                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(w, total_h),
                    Sense::click_and_drag(),
                );
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, PANEL);

                // Time ruler.
                let sec_px = w / (usize_dur as f32 / SECOND_US as f32).max(1.0);
                let mut s = 0;
                let mut x = 0.0;
                while x < w {
                    painter.text(
                        egui::pos2(rect.min.x + x + 2.0, rect.min.y + 2.0),
                        egui::Align2::LEFT_TOP,
                        format!("{s}s"),
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(150, 150, 160),
                    );
                    s += 1;
                    x += sec_px;
                }

                // Track lanes.
                let mut clicks: Vec<(usize, usize, u8, Rect)> = Vec::new();
                // Simplify: draw lanes inline below.
                let mut lane_rects: Vec<(u8, usize, Rect)> = Vec::new();

                // Video lanes.
                for (ti, tr) in self.project.clone().video_tracks.iter().enumerate() {
                    let y0 = rect.min.y + ruler_h + ti as f32 * row_h;
                    let lane = Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(rect.max.x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.text(
                        lane.min + egui::vec2(4.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        tr.name.clone(),
                        egui::FontId::proportional(11.0),
                        Color32::from_rgb(180, 180, 190),
                    );
                    lane_rects.push((0, ti, lane));
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw = ((c.source_out - c.source_in) as f32 / usize_dur as f32 * w)
                            .max(6.0);
                        let cx = (c.timeline_start as f32 / usize_dur as f32 * w).max(0.0);
                        let cr = Rect::from_min_max(
                            egui::pos2(rect.min.x + cx + 2.0, y0 + 2.0),
                            egui::pos2(rect.min.x + cx + cw - 2.0, y0 + row_h - 5.0),
                        );
                        painter.rect_filled(cr, 2.0, VIDEO_CLIP);
                        painter.text(
                            cr.min + egui::vec2(4.0, 4.0),
                            egui::Align2::LEFT_TOP,
                            &c_name(&self.project, c.asset),
                            egui::FontId::proportional(10.0),
                            WHITE,
                        );
                        if let Some(p) = response.hover_pos() {
                            if cr.contains(p) && response.clicked() {
                                clicks.push((ti, ci, 0u8, cr));
                            }
                        }
                    }
                }

                // Audio lanes.
                for (ai, tr) in self.project.clone().audio_tracks.iter().enumerate() {
                    let y0 = rect.min.y + ruler_h + (vid_rows + ai) as f32 * row_h;
                    let lane = Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(rect.max.x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.text(
                        lane.min + egui::vec2(4.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        tr.name.clone(),
                        egui::FontId::proportional(11.0),
                        Color32::from_rgb(180, 180, 190),
                    );
                    lane_rects.push((1, ai, lane));
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw = ((c.source_out - c.source_in) as f32 / usize_dur as f32 * w)
                            .max(6.0);
                        let cx = (c.timeline_start as f32 / usize_dur as f32 * w).max(0.0);
                        let cr = Rect::from_min_max(
                            egui::pos2(rect.min.x + cx + 2.0, y0 + 4.0),
                            egui::pos2(rect.min.x + cx + cw - 2.0, y0 + row_h - 7.0),
                        );
                        painter.rect_filled(cr, 2.0, AUDIO_CLIP);
                        if let Some(asset) = self.project.assets.get(c.asset) {
                            if self.settings.show_peaks && !asset.peaks.is_empty() {
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
                        if let Some(p) = response.hover_pos() {
                            if cr.contains(p) && response.clicked() {
                                clicks.push((ai, ci, 1u8, cr));
                            }
                        }
                    }
                }

                // Text lanes.
                for (ti, tr) in self.project.clone().text_tracks.iter().enumerate() {
                    let y0 = rect.min.y + ruler_h + (vid_rows + aud_rows + ti) as f32 * row_h;
                    let lane = Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(rect.max.x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.text(
                        lane.min + egui::vec2(4.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        tr.name.clone(),
                        egui::FontId::proportional(11.0),
                        Color32::from_rgb(180, 180, 190),
                    );
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw = ((c.timeline_end - c.timeline_start) as f32 / usize_dur as f32 * w)
                            .max(6.0);
                        let cx = (c.timeline_start as f32 / usize_dur as f32 * w).max(0.0);
                        let cr = Rect::from_min_max(
                            egui::pos2(rect.min.x + cx + 2.0, y0 + 4.0),
                            egui::pos2(rect.min.x + cx + cw - 2.0, y0 + row_h - 7.0),
                        );
                        painter.rect_filled(cr, 2.0, TEXT_CLIP);
                        if let Some(p) = response.hover_pos() {
                            if cr.contains(p) && response.clicked() {
                                clicks.push((ti, ci, 2u8, cr));
                            }
                        }
                    }
                }

                // Selection on click.
                if let Some((ti, ci, kind, _)) = clicks.last().copied() {
                    self.selected = match kind {
                        0 => Selection::Video { track: ti, clip: ci },
                        1 => Selection::Audio { track: ti, clip: ci },
                        _ => Selection::Text { track: ti, clip: ci },
                    };
                }

                // Playhead.
                let px = rect.min.x
                    + (self.playhead_us as f32 / usize_dur as f32 * w).clamp(0.0, w);
                painter.line_segment(
                    [egui::pos2(px, rect.min.y), egui::pos2(px, rect.max.y)],
                    egui::Stroke::new(2.0, PLAYHEAD),
                );

                // Click on ruler seeks.
                if response.clicked() {
                    if let Some(p) = response.hover_pos() {
                        if p.y < rect.min.y + ruler_h {
                            let frac = ((p.x - rect.min.x) / w).clamp(0.0, 1.0);
                            self.playhead_us = (usize_dur as f64 * frac as f64) as i64;
                            self.seek_marker();
                        }
                    }
                }

                // Drag-drop from the media panel: while dragging show a ghost,
                // on release add the clip at the drop position/lane.
                if let Some(src) = self.drag_source {
                    let dragging = ctx.input(|i| i.pointer.primary_down());
                    if dragging {
                        if let Some(p) = ctx.input(|i| i.pointer.hover_pos()) {
                            if rect.contains(p) {
                                let frac = ((p.x - rect.min.x) / w).clamp(0.0, 1.0);
                                let drop_us = (usize_dur as f64 * frac as f64) as i64;
                                self.drag_timeline_us = Some(drop_us);
                                // Ghost.
                                let gx = rect.min.x + frac * w;
                                painter.line_segment(
                                    [
                                        egui::pos2(gx, rect.min.y),
                                        egui::pos2(gx, rect.max.y),
                                    ],
                                    egui::Stroke::new(1.0, ACCENT),
                                );
                            }
                        }
                    }
                    if ctx.input(|i| i.pointer.any_released()) {
                        let mut drop_us =
                            self.drag_timeline_us.unwrap_or(self.project.duration_us());
                        // Snap to the playhead when close (150ms window).
                        if self.settings.snap_to_playhead
                            && (drop_us - self.playhead_us).abs() < 150_000
                        {
                            drop_us = self.playhead_us;
                        }
                        if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                            if rect.contains(p) {
                                // Find lane under the pointer.
                                let mut target: Option<(u8, usize)> = None;
                                for (kind, idx, lane) in &lane_rects {
                                    if lane.contains(p) {
                                        target = Some((*kind, *idx));
                                        break;
                                    }
                                }
                                if let Some((kind, idx)) = target {
                                    self.push_undo();
                                    self.add_clip_at(src, kind, idx, drop_us);
                                }
                            }
                        }
                        self.drag_source = None;
                        self.drag_timeline_us = None;
                    }
                }
            });
    }

    // ── Import / asset helpers ───────────────────────────────────────────────
    fn pick_and_import(&mut self) {
        let path = rfd::FileDialog::new()
            .set_title("Import media")
            .add_filter(
                "Media",
                &[
                    "mp4", "mov", "mkv", "webm", "avi", "mp3", "wav", "aac", "flac", "ogg",
                    "png", "jpg", "jpeg", "webp",
                ],
            )
            .pick_file();
        if let Some(p) = path {
            self.import_media(p);
        }
    }

    fn import_media(&mut self, path: PathBuf) {
        let path_s = path.to_string_lossy().into_owned();
        match crate::assets::import_media(&mut self.project.assets, &path_s) {
            Ok(id) => {
                // Auto-adjust the project frame rate to the source video.
                let mut tag = String::new();
                if let Some(a) = self.project.assets.get(id) {
                    if a.kind == AssetKind::Video && a.frame_rate > 0.0 {
                        self.project.fps = a.frame_rate;
                        tag = format!(" at {:.2} fps", a.frame_rate);
                    }
                }
                self.audio.refresh(&self.project);
                self.toasts.push((
                    format!(
                        "Imported {}{}",
                        self.project
                            .assets
                            .get(id)
                            .map(|a| a.name.clone())
                            .unwrap_or_default(),
                        tag
                    ),
                    Instant::now(),
                ));
            }
            Err(e) => self.toasts.push((format!("Import failed: {e}"), Instant::now())),
        }
    }

    fn remove_asset(&mut self, id: AssetId) {
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
            self.toasts.push(("Remove clips first".into(), Instant::now()));
            return;
        }
        self.project.assets.assets.remove(&id);
        self.thumbnails.remove(&id);
        self.audio.refresh(&self.project);
    }

    fn add_clip_from_media(&mut self, id: AssetId) {
        let Some(asset) = self.project.assets.get(id).cloned() else {
            return;
        };
        let start = self.project.duration_us();
        match asset.kind {
            AssetKind::Video => self.add_clip_at(id, 0, 0, start),
            AssetKind::Image => self.add_clip_at(id, 0, 0, start),
            AssetKind::Audio => self.add_clip_at(id, 1, 0, start),
        }
        if asset.kind != AssetKind::Image {
            self.audio.refresh(&self.project);
        }
        self.last_request = None;
    }

    /// Insert an asset as a clip on lane `kind` (0=video, 1=audio) at `time_us`.
    fn add_clip_at(&mut self, id: AssetId, kind: u8, track: usize, time_us: i64) {
        let Some(asset) = self.project.assets.get(id).cloned() else {
            return;
        };
        let dur = asset.duration_us.max(SECOND_US);
        match kind {
            0 => {
                let idx = track.min(self.project.video_tracks.len().saturating_sub(1));
                match asset.kind {
                    AssetKind::Image => {
                        self.project.video_tracks[idx].clips.push(VideoClip {
                            asset: AssetId(id.0),
                            source_in: 0,
                            source_out: 3 * SECOND_US,
                            timeline_start: time_us,
                            opacity: 1.0,
                            transform: Transform::default(),
                        });
                    }
                    _ => {
                        self.project.video_tracks[idx].clips.push(VideoClip {
                            asset: AssetId(id.0),
                            source_in: 0,
                            source_out: dur,
                            timeline_start: time_us,
                            opacity: 1.0,
                            transform: Transform::default(),
                        });
                    }
                }
            }
            1 => {
                let idx = track.min(self.project.audio_tracks.len().saturating_sub(1));
                self.project.audio_tracks[idx].clips.push(AudioClip {
                    asset: AssetId(id.0),
                    source_in: 0,
                    source_out: dur,
                    timeline_start: time_us,
                    gain_db: 0.0,
                    fade_in_us: 200_000,
                    fade_out_us: 200_000,
                });
            }
            _ => {}
        }
        self.last_request = None;
    }

    fn add_text_clip(&mut self) {
        let start = self.playhead_us;
        let end = start + 3 * SECOND_US;
        self.push_undo();
        self.project.text_tracks[0].clips.push(TextClip {
            text: "New title".into(),
            timeline_start: start,
            timeline_end: end,
            style: TextStyle::default(),
            transform: Transform::default(),
            opacity: 1.0,
        });
    }

    fn split_selected(&mut self) {
        match self.selected {
            Selection::Video { track, clip } => self.split_video(track, clip),
            Selection::Audio { track, clip } => self.split_audio(track, clip),
            _ => {}
        }
    }

    // ── Inspector (right panel) ──────────────────────────────────────────────
    fn inspector(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("inspector")
            .resizable(true)
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.strong("Inspector");
                ui.separator();
                match self.selected {
                    Selection::None => {
                        ui.label("Select a clip to edit.");
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
            });
    }

    fn inspect_video(&mut self, ui: &mut egui::Ui, track: usize, clip: usize) {
        let Some(tr) = self.project.video_tracks.get(track) else {
            return;
        };
        let Some(c) = tr.clips.get(clip).cloned() else {
            return;
        };
        let s = timecode_string(c.source_out - c.source_in, self.project.fps);
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
                .text("Rotate deg")
                .fixed_decimals(0),
        );
        let mut opacity = c.opacity;
        if ui
            .add(egui::Slider::new(&mut opacity, 0.0..=1.0).text("Opacity"))
            .changed()
        {
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
        if ui.button("Delete clip").clicked() {
            self.push_undo();
            self.project.video_tracks[track].clips.remove(clip);
            self.selected = Selection::None;
            self.audio.refresh(&self.project);
        }
    }

    fn inspect_audio(&mut self, ui: &mut egui::Ui, track: usize, clip: usize) {
        let Some(tr) = self.project.audio_tracks.get(track) else {
            return;
        };
        let Some(c) = tr.clips.get(clip).cloned() else {
            return;
        };
        let s = timecode_string(c.source_out - c.source_in, self.project.fps);
        ui.label(format!("Duration: {s}"));
        let mut gain = c.gain_db;
        if ui
            .add(egui::Slider::new(&mut gain, -60.0..=12.0).text("Gain dB"))
            .changed()
        {
            if let Some(c) = self.project.audio_tracks[track].clips.get_mut(clip) {
                c.gain_db = gain;
            }
        }
        let mut fi = c.fade_in_us;
        let mut fo = c.fade_out_us;
        if ui
            .add(egui::Slider::new(&mut fi, 0..=5_000_000).text("Fade in (ms)"))
            .changed()
        {
            if let Some(c) = self.project.audio_tracks[track].clips.get_mut(clip) {
                c.fade_in_us = fi;
            }
        }
        if ui
            .add(egui::Slider::new(&mut fo, 0..=5_000_000).text("Fade out (ms)"))
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
        if ui.button("Delete clip").clicked() {
            self.push_undo();
            self.project.audio_tracks[track].clips.remove(clip);
            self.selected = Selection::None;
            self.audio.refresh(&self.project);
        }
    }

    fn inspect_text(&mut self, ui: &mut egui::Ui, track: usize, clip: usize) {
        let Some(tr) = self.project.text_tracks.get(track) else {
            return;
        };
        let Some(c) = tr.clips.get(clip).cloned() else {
            return;
        };
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
        if ui
            .add(egui::Slider::new(&mut size, 8.0..=120.0).text("Size"))
            .changed()
        {
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
        ui.separator();
        if ui.button("Delete clip").clicked() {
            self.push_undo();
            self.project.text_tracks[track].clips.remove(clip);
            self.selected = Selection::None;
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
        self.push_undo();
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
        self.push_undo();
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

    // ── Export dialog ────────────────────────────────────────────────────────
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

    // ── Preferences dialog (editable keybinds + global settings) ─────────────
    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        let prev_dark = self.settings.dark_mode;
        let mut reset_binds = false;
        let mut request_close = false;
        egui::Window::new("Preferences")
            .open(&mut open)
            .collapsible(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.heading("Keyboard shortcuts");
                ui.label("Click a shortcut, then press the new keys (Esc to cancel).");
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        egui::Grid::new("settings_binds")
                            .num_columns(2)
                            .spacing([12.0, 4.0])
                            .show(ui, |ui| {
                                for (label, f) in BIND_ROWS {
                                    ui.label(label);
                                    let capturing = self.capturing == Some(f);
                                    let text = if capturing {
                                        "press keys…".to_string()
                                    } else {
                                        shortcut_label(self.keybinds.get(f))
                                    };
                                    let btn = egui::Button::new(text).min_size(egui::vec2(150.0, 22.0));
                                    if ui.add(btn).clicked() {
                                        self.capturing = Some(f);
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.horizontal(|ui| {
                    if ui.button("Reset keybinds").clicked() {
                        reset_binds = true;
                    }
                    if self.capturing.is_some() && ui.button("Cancel capture").clicked() {
                        self.capturing = None;
                    }
                });
                ui.separator();
                ui.heading("Application");
                ui.checkbox(&mut self.settings.dark_mode, "Dark mode");
                ui.checkbox(&mut self.settings.show_peaks, "Show audio peaks in the timeline");
                ui.checkbox(
                    &mut self.settings.snap_to_playhead,
                    "Snap media drops to the playhead",
                );
                ui.horizontal(|ui| {
                    ui.label("Preview render size");
                    ui.add(
                        egui::DragValue::new(&mut self.settings.preview_width)
                            .range(320..=4096)
                            .speed(2),
                    );
                    ui.label("x");
                    ui.add(
                        egui::DragValue::new(&mut self.settings.preview_height)
                            .range(180..=4096)
                            .speed(2),
                    );
                });
                ui.label("Applied on the next rendered frame.");
                ui.separator();
                request_close = ui.button("Close").clicked();
            });
        if reset_binds {
            self.keybinds.reset();
            self.capturing = None;
            self.toasts.push(("Keybinds reset to defaults".into(), Instant::now()));
        }
        if prev_dark != self.settings.dark_mode {
            apply_theme(ctx, self.settings.dark_mode);
            self.toasts.push((
                if self.settings.dark_mode {
                    "Dark mode enabled".into()
                } else {
                    "Light mode enabled".into()
                },
                Instant::now(),
            ));
        }
        self.settings_open = open && !request_close;
    }

    // ── Toast overlay (top-right, auto-fading) ──────────────────────────────
    fn toast_overlay(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|(_, t)| t.elapsed() < Duration::from_secs(3));
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts_area"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 44.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::window(ui.style())
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(1.0, TRACK))
                    .corner_radius(egui::CornerRadius::same(4))
                    .show(ui, |ui| {
                        for (msg, _) in &self.toasts {
                            ui.colored_label(Color32::from_rgb(120, 220, 160), msg);
                        }
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

fn shortcut_label(sc: KeyboardShortcut) -> String {
    let mut parts = Vec::new();
    if sc.modifiers.ctrl {
        parts.push("Ctrl");
    }
    if sc.modifiers.shift {
        parts.push("Shift");
    }
    if sc.modifiers.alt {
        parts.push("Alt");
    }
    let key = match sc.logical_key {
        Key::Space => "Space".to_string(),
        Key::ArrowLeft => "Left".to_string(),
        Key::ArrowRight => "Right".to_string(),
        Key::Home => "Home".to_string(),
        Key::End => "End".to_string(),
        Key::Escape => "Esc".to_string(),
        Key::Delete => "Delete".to_string(),
        other => format!("{other:?}"),
    };
    parts.push(&key);
    parts.join("+")
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

fn apply_theme(ctx: &egui::Context, dark: bool) {
    let mut style = (*ctx.style()).clone();
    style.visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    if dark {
        style.visuals.panel_fill = PANEL;
        style.visuals.window_fill = PANEL;
        style.visuals.extreme_bg_color = TRACK;
        style.visuals.faint_bg_color = Color32::from_rgb(10, 10, 12);
        style.visuals.override_text_color = Some(WHITE);
        style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, WHITE);
        style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, WHITE);
        style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, WHITE);
        style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5, WHITE);
        style.visuals.selection.bg_fill = Color32::from_rgb(40, 80, 140);
    } else {
        let fg = Color32::from_rgb(20, 20, 26);
        style.visuals.panel_fill = Color32::from_rgb(243, 243, 248);
        style.visuals.window_fill = Color32::from_rgb(250, 250, 252);
        style.visuals.extreme_bg_color = Color32::from_rgb(226, 226, 234);
        style.visuals.faint_bg_color = Color32::from_rgb(236, 236, 242);
        style.visuals.override_text_color = Some(fg);
        style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, fg);
        style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, fg);
        style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, fg);
        style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5, fg);
        style.visuals.selection.bg_fill = Color32::from_rgb(160, 200, 255);
    }
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    ctx.set_style(style);
}