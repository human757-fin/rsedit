//! rsedit — the editor application itself.
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
    Transform, VideoClip, SECOND_US, TRACK_COLORS,
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

    /// Serialize all binds as `(label, ctrl, shift, alt, key name)` for prefs.
    fn binds(&self) -> Vec<(String, bool, bool, bool, String)> {
        BIND_ROWS
            .iter()
            .map(|(label, f)| {
                let sc = self.get(*f);
                (
                    (*label).to_string(),
                    sc.modifiers.ctrl,
                    sc.modifiers.shift,
                    sc.modifiers.alt,
                    format!("{:?}", sc.logical_key),
                )
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BindField {
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

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) struct Settings {
    show_peaks: bool,
    dark_mode: bool,
    snap_to_playhead: bool,
    preview_width: u32,
    preview_height: u32,
    #[serde(default)]
    safe_margins: bool,
    #[serde(default)]
    accent: [u8; 3],
    #[serde(default)]
    autosave: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_peaks: true,
            dark_mode: true,
            snap_to_playhead: true,
            preview_width: 1280,
            preview_height: 720,
            safe_margins: false,
            accent: [240, 120, 64],
            autosave: true,
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

pub struct RsEditApp {
    project: Project,
    cache: FrameCache,
    audio: AudioEngine,
    playhead_us: Timecode,
    playing: bool,
    last_request: Option<(u64, i64)>,
    preview_texture: Option<egui::TextureHandle>,
    preview_rendered_pts: Option<i64>,
    selected: Selection,
    multi_selection: Vec<Selection>,
    clipboard: Option<ClipboardItem>,
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
    // Timeline view state
    timeline_scroll_us: Timecode,
    timeline_zoom: f32,
    timeline_drag: Option<TimelineDrag>,
    // Export dialog
    export_open: bool,
    export_path: String,
    export_w: u32,
    export_h: u32,
    export_fps: f64,
    export_bitrate: u32,
    export_codec: u8,
    export_audio_only: bool,
    export_range: bool,
    export_msg: Option<String>,
    export_worker: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    export_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    export_progress_cell: std::sync::Arc<std::sync::Mutex<f32>>,
    export_running: bool,
    export_progress: f32,
    /// Source fps awaiting user confirmation to adopt as the project fps.
    pending_fps: Option<f64>,
    toasts: Vec<(String, Instant)>,
    // Windows / overlays
    mixer_open: bool,
    palette_open: bool,
    palette_filter: String,
    palette_sel: usize,
    fullscreen_preview: bool,
    preview_zoom: f32,
    relink_open: bool,
    recent_files: Vec<String>,
    /// Where the current project was last saved (None = never saved).
    current_project_path: Option<String>,
    // Undo / redo stacks
    undo_stack: Vec<Project>,
    redo_stack: Vec<Project>,
    // Autosave
    autosave_timer: Instant,
    dirty: bool,
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

#[derive(Clone)]
enum ClipboardItem {
    Video(VideoClip),
    Audio(AudioClip),
    Text(TextClip),
}

#[derive(Clone, Copy)]
enum TimelineDrag {
    Move { kind: u8, track: usize, clip: usize, grab_us: i64 },
    TrimL { kind: u8, track: usize, clip: usize },
    TrimR { kind: u8, track: usize, clip: usize },
}

#[derive(Clone, Copy)]
enum PaletteAction {
    Import,
    NewProject,
    OpenProject,
    SaveProject,
    SaveAs,
    Export,
    AddVideoTrack,
    AddAudioTrack,
    AddText,
    Mark,
    Split,
    DeleteSel,
    Loop,
    Ripple,
    Fullscreen,
    Mixer,
    Snap,
    Margins,
    Dark,
    Preferences,
}

impl RsEditApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ensure_ffmpeg();
        let prefs = crate::prefs::load();
        let settings = prefs
            .as_ref()
            .map(|p| p.settings)
            .unwrap_or_default();
        let keybinds = prefs
            .as_ref()
            .map(|p| {
                let mut kb = Keybinds::default();
                for (name, ctrl, shift, alt, key) in &p.keybinds {
                    if let Some(f) = crate::prefs::keybind_field(name) {
                        if let Some(k) = crate::prefs::key_from_str(key) {
                            let mut m = Modifiers::NONE;
                            m.ctrl = *ctrl;
                            m.shift = *shift;
                            m.alt = *alt;
                            kb.set(f, KeyboardShortcut::new(m, k));
                        }
                    }
                }
                kb
            })
            .unwrap_or_default();
        let dark = settings.dark_mode;
        apply_theme(&cc.egui_ctx, dark, settings.accent);
        let project = Project::new();
        let audio = AudioEngine::new(&project);
        let cache = FrameCache::new();
        let recent = prefs.as_ref().map(|p| p.recent.clone()).unwrap_or_default();
        let mut app = Self {
            project,
            cache,
            audio,
            playhead_us: 0,
            playing: false,
            last_request: None,
            preview_texture: None,
            preview_rendered_pts: None,
            selected: Selection::None,
            multi_selection: Vec::new(),
            clipboard: None,
            media_tab: MediaTab::All,
            media_filter: String::new(),
            thumbnails: HashMap::new(),
            drag_source: None,
            drag_timeline_us: None,
            settings_open: false,
            capturing: None,
            keybinds,
            settings,
            timeline_scroll_us: 0,
            timeline_zoom: 1.0,
            timeline_drag: None,
            export_open: false,
            export_path: "out.mp4".into(),
            export_w: 1920,
            export_h: 1080,
            export_fps: 30.0,
            export_bitrate: 8000,
            export_codec: 0,
            export_audio_only: false,
            export_range: false,
            export_msg: None,
            export_worker: None,
            export_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            export_progress_cell: std::sync::Arc::new(std::sync::Mutex::new(0.0)),
            export_running: false,
            export_progress: 0.0,
            pending_fps: None,
            toasts: Vec::new(),
            mixer_open: false,
            palette_open: false,
            palette_filter: String::new(),
            palette_sel: 0,
            fullscreen_preview: false,
            preview_zoom: 1.0,
            relink_open: false,
            recent_files: recent,
            current_project_path: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            autosave_timer: Instant::now(),
            dirty: false,
        };
        // Attempt to recover an autosaved project.
        if let Some(p) = crate::prefs::load_autosave() {
            app.project = p;
            app.audio.refresh(&app.project);
            app.toasts.push(("Recovered autosaved project".into(), Instant::now()));
        }
        app
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.project.clone());
        self.redo_stack.clear();
        if self.undo_stack.len() > 100 {
            self.undo_stack.remove(0);
        }
        self.dirty = true;
    }

    fn accent(&self) -> Color32 {
        let a = self.settings.accent;
        Color32::from_rgb(a[0], a[1], a[2])
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
                let removed_start = match sel {
                    Selection::Video { track, clip } => {
                        self.project.video_tracks[track].clips[clip].timeline_start
                    }
                    Selection::Audio { track, clip } => {
                        self.project.audio_tracks[track].clips[clip].timeline_start
                    }
                    _ => self.project.text_tracks[track].clips[clip].timeline_start,
                };
                let removed_len = match sel {
                    Selection::Video { track, clip } => {
                        self.project.video_tracks[track].clips[clip].on_timeline_us()
                    }
                    Selection::Audio { track, clip } => {
                        self.project.audio_tracks[track].clips[clip].on_timeline_us()
                    }
                    _ => {
                        self.project.text_tracks[track].clips[clip].timeline_end
                            - self.project.text_tracks[track].clips[clip].timeline_start
                    }
                };
                if kind == 0 {
                    self.project.video_tracks[track].clips.remove(clip);
                } else if kind == 1 {
                    self.project.audio_tracks[track].clips.remove(clip);
                } else {
                    self.project.text_tracks[track].clips.remove(clip);
                }
                self.selected = Selection::None;
                // Ripple: shift clips after the removed gap on the same track.
                if self.project.ripple && removed_len > 0 {
                    match kind {
                        0 => {
                            for c in self.project.video_tracks[track].clips.iter_mut() {
                                if c.timeline_start >= removed_start {
                                    c.timeline_start = (c.timeline_start - removed_len).max(0);
                                }
                            }
                        }
                        1 => {
                            for c in self.project.audio_tracks[track].clips.iter_mut() {
                                if c.timeline_start >= removed_start {
                                    c.timeline_start = (c.timeline_start - removed_len).max(0);
                                }
                            }
                        }
                        _ => {
                            for c in self.project.text_tracks[track].clips.iter_mut() {
                                if c.timeline_start >= removed_start {
                                    let len = c.timeline_end - c.timeline_start;
                                    c.timeline_start = (c.timeline_start - removed_len).max(0);
                                    c.timeline_end = c.timeline_start + len;
                                }
                            }
                        }
                    }
                }
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
                    color: TRACK_COLORS[(n - 1) % TRACK_COLORS.len()],
                    ..Default::default()
                });
            }
            1 => {
                let n = self.project.audio_tracks.len() + 1;
                self.project.audio_tracks.push(Track {
                    name: format!("A{n}"),
                    color: TRACK_COLORS[(n - 1) % TRACK_COLORS.len()],
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
            // Fixed app-wide shortcuts (not user-rebindable).
            let ctrl = i.modifiers.ctrl;
            let shift = i.modifiers.shift;
            if ctrl && !shift {
                if i.key_pressed(Key::C) {
                    self.copy_selected();
                } else if i.key_pressed(Key::X) {
                    self.cut_selected();
                } else if i.key_pressed(Key::V) {
                    self.paste_clip();
                } else if i.key_pressed(Key::K) {
                    self.toggle_palette();
                } else if i.key_pressed(Key::M) {
                    self.mixer_open = !self.mixer_open;
                }
            } else if shift && !ctrl {
                let arrow = if i.key_pressed(Key::ArrowLeft) {
                    Some(-1)
                } else if i.key_pressed(Key::ArrowRight) {
                    Some(1)
                } else {
                    None
                };
                if let Some(dir) = arrow {
                    self.nudge_selected(dir);
                }
            }
            if i.key_pressed(Key::F11) {
                self.fullscreen_preview = !self.fullscreen_preview;
            }
        });
    }

    fn toggle_palette(&mut self) {
        self.palette_open = !self.palette_open;
        if self.palette_open {
            self.palette_sel = 0;
        }
    }

    fn copy_selected(&mut self) {
        match self.selected {
            Selection::Video { track, clip } => {
                if let Some(c) = self.project.video_tracks[track].clips.get(clip) {
                    self.clipboard = Some(ClipboardItem::Video(c.clone()));
                    self.toasts.push(("Copied clip".into(), Instant::now()));
                }
            }
            Selection::Audio { track, clip } => {
                if let Some(c) = self.project.audio_tracks[track].clips.get(clip) {
                    self.clipboard = Some(ClipboardItem::Audio(c.clone()));
                    self.toasts.push(("Copied clip".into(), Instant::now()));
                }
            }
            Selection::Text { track, clip } => {
                if let Some(c) = self.project.text_tracks[track].clips.get(clip) {
                    self.clipboard = Some(ClipboardItem::Text(c.clone()));
                    self.toasts.push(("Copied clip".into(), Instant::now()));
                }
            }
            _ => {}
        }
    }

    fn cut_selected(&mut self) {
        self.copy_selected();
        self.delete_selected();
    }

    fn paste_clip(&mut self) {
        let Some(item) = self.clipboard.clone() else {
            return;
        };
        let at = self.playhead_us;
        self.push_undo();
        match item {
            ClipboardItem::Video(mut c) => {
                c.timeline_start = at;
                self.project.video_tracks[0].clips.push(c);
            }
            ClipboardItem::Audio(mut c) => {
                c.timeline_start = at;
                self.project.audio_tracks[0].clips.push(c);
            }
            ClipboardItem::Text(mut c) => {
                let len = c.timeline_end - c.timeline_start;
                let end = if len > 0 { at + len } else { at + 3 * SECOND_US };
                c.timeline_start = at;
                c.timeline_end = end;
                self.project.text_tracks[0].clips.push(c);
            }
        }
        self.audio.refresh(&self.project);
        self.last_request = None;
        self.toasts.push(("Pasted clip at playhead".into(), Instant::now()));
    }

    /// Nudge the selected clip left/right by one frame (Shift+arrows).
    fn nudge_selected(&mut self, dir: i64) {
        let per = (SECOND_US as f64 / self.project.fps).max(1.0) as i64;
        let delta = dir * per;
        match self.selected {
            Selection::Video { track, clip } => {
                if let Some(c) = self.project.video_tracks[track].clips.get_mut(clip) {
                    c.timeline_start = (c.timeline_start + delta).max(0);
                }
            }
            Selection::Audio { track, clip } => {
                if let Some(c) = self.project.audio_tracks[track].clips.get_mut(clip) {
                    c.timeline_start = (c.timeline_start + delta).max(0);
                }
            }
            Selection::Text { track, clip } => {
                if let Some(c) = self.project.text_tracks[track].clips.get_mut(clip) {
                    let len = c.timeline_end - c.timeline_start;
                    c.timeline_start = (c.timeline_start + delta).max(0);
                    c.timeline_end = c.timeline_start + len;
                }
            }
            _ => return,
        }
        self.dirty = true;
        self.audio.refresh(&self.project);
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

fn selection_equals(a: Selection, b: Selection) -> bool {
    a == b
}

fn clip_time_start(p: &Project, kind: u8, track: usize, clip: usize) -> i64 {
    match kind {
        0 => p
            .video_tracks
            .get(track)
            .and_then(|t| t.clips.get(clip))
            .map(|c| c.timeline_start)
            .unwrap_or(0),
        1 => p
            .audio_tracks
            .get(track)
            .and_then(|t| t.clips.get(clip))
            .map(|c| c.timeline_start)
            .unwrap_or(0),
        _ => p
            .text_tracks
            .get(track)
            .and_then(|t| t.clips.get(clip))
            .map(|c| c.timeline_start)
            .unwrap_or(0),
    }
}

fn top_video_clip(p: &Project, t: Timecode) -> Option<(usize, &VideoClip)> {
    p.video_tracks.iter().enumerate().rev().find_map(|(ti, tr)| {
        tr.clips
            .iter()
            .find(|c| c.timeline_start <= t && t < c.timeline_start + c.on_timeline_us())
            .map(|c| (ti, c))
    })
}

// ──────────────────────────────────────────────────────────────────────────────
//  eframe app loop
// ──────────────────────────────────────────────────────────────────────────────

impl eframe::App for RsEditApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Transport clock driven by audio engine when playing.
        if self.playing {
            let pos = self.audio.pos_us();
            if pos > self.playhead_us as f64 {
                self.playhead_us = pos as i64;
            }
            // Loop region: wrap playback back to the loop start instead of
            // stopping at the end of the project.
            if let (Some(ls), Some(le)) =
                (self.project.loop_start, self.project.loop_end)
            {
                if le > ls
                    && self.playhead_us >= le
                    && self.playhead_us < self.project.duration_us()
                {
                    self.playhead_us = ls;
                    self.audio.set_pos_us(ls as f64);
                    self.playhead_us = self.audio.pos_us() as i64;
                }
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
        if self.fullscreen_preview {
            self.fullscreen_preview_window(ctx);
        } else {
            self.timeline(ctx);
            self.media_panel(ctx);
            self.inspector(ctx);
            self.preview_panel(ctx);
        }
        if self.palette_open {
            self.palette_window(ctx);
        }
        if self.mixer_open {
            self.mixer_window(ctx);
        }
        if self.relink_open {
            self.relink_window(ctx);
        }
        if self.export_open {
            self.export_window(ctx);
        }
        if self.settings_open {
            self.settings_window(ctx);
        }
        // Reap a finished export worker thread.
        if self.export_running {
            if let Some(h) = self.export_worker.take() {
                if h.is_finished() {
                    self.export_running = false;
                    *self.export_progress_cell.lock().unwrap() = 1.0;
                    match h.join() {
                        Ok(Ok(())) => {
                            self.export_msg = Some("Export complete.".into());
                            self.export_progress = 1.0;
                        }
                        Ok(Err(e)) => {
                            if e.to_string() == "export cancelled" {
                                self.export_msg = Some("Export cancelled.".into());
                                self.export_progress = 0.0;
                            } else {
                                self.export_msg = Some(format!("Export failed: {e:#}"));
                                self.export_progress = 0.0;
                            }
                        }
                        Err(_) => {
                            self.export_msg = Some("Export thread panicked.".into());
                            self.export_progress = 0.0;
                        }
                    }
                } else {
                    self.export_worker = Some(h);
                }
            }
            self.export_progress = *self.export_progress_cell.lock().unwrap();
        }
        // Autosave every ~10s while the project is dirty.
        if self.settings.autosave
            && self.dirty
            && self.autosave_timer.elapsed().as_secs() >= 10
        {
            crate::prefs::save_autosave(&self.project);
            self.autosave_timer = Instant::now();
        }
        self.fps_prompt_window(ctx);
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

impl RsEditApp {
    // ── Menu bar ─────────────────────────────────────────────────────────────
    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.add(egui::Button::new("New project")).clicked() {
                        ui.close_menu();
                        self.new_project();
                    }
                    if ui.add(egui::Button::new("Open project...")).clicked() {
                        ui.close_menu();
                        self.prompt_open_project();
                    }
                    if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                        ui.close_menu();
                        self.save_project();
                    }
                    if ui.add(egui::Button::new("Save as...")).clicked() {
                        ui.close_menu();
                        self.save_project_as();
                    }
                    if !self.recent_files.is_empty() {
                        ui.menu_button("Open recent", |ui| {
                            for p in self.recent_files.clone() {
                                if ui.button(&format!("␣ {p}")).clicked() {
                                    ui.close_menu();
                                    self.load_project_file(&p);
                                }
                            }
                            ui.separator();
                            if ui.button("Clear recent files").clicked() {
                                self.recent_files.clear();
                                let _ = crate::prefs::write_config(
                                    self.settings,
                                    &self.keybinds.binds(),
                                    &self.recent_files,
                                );
                            }
                        });
                    }
                    ui.separator();
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
                ui.menu_button("View", |ui| {
                    if ui
                        .add(egui::Button::new("Command palette").shortcut_text("Ctrl+K"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.toggle_palette();
                    }
                    if ui
                        .add(egui::Button::new("Mixer window").shortcut_text("Ctrl+M"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.mixer_open = !self.mixer_open;
                    }
                    if ui.button("Fullscreen preview").clicked() {
                        ui.close_menu();
                        self.fullscreen_preview = !self.fullscreen_preview;
                    }
                    ui.separator();
                    ui.label(format!(
                        "Loop {}· Ripple {}",
                        if self.project.loop_start.is_some() {
                            "on"
                        } else {
                            "off"
                        },
                        if self.project.ripple { "on" } else { "off" }
                    ));
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

                let looping =
                    self.project.loop_start.is_some() && self.project.loop_end.is_some();
                if ui
                    .add_sized(
                        [58.0, 26.0],
                        egui::SelectableLabel::new(looping, "Loop"),
                    )
                    .on_hover_text("Loop playback between the loop start and end points")
                    .clicked()
                {
                    if looping {
                        self.push_undo();
                        self.project.loop_start = None;
                        self.project.loop_end = None;
                    } else if self.project.duration_us() > 0 {
                        self.push_undo();
                        let s = (self.playhead_us).min(
                            (self.project.duration_us() - SECOND_US).max(0),
                        );
                        let e = (s + 4 * SECOND_US).min(self.project.duration_us()).max(s + 1);
                        self.project.loop_start = Some(s);
                        self.project.loop_end = Some(e);
                    }
                }
                if ui
                    .add_sized([60.0, 26.0], egui::Button::new("Marker").fill(BUTTON))
                    .on_hover_text("Place a marker at the playhead")
                    .clicked()
                {
                    self.push_undo();
                    self.project.markers.push(crate::timeline::Marker {
                        time_us: self.playhead_us,
                        name: String::new(),
                        color: [240, 180, 60],
                    });
                    self.project.markers.sort_by_key(|m| m.time_us);
                }
                if ui
                    .add_sized(
                        [58.0, 26.0],
                        egui::SelectableLabel::new(self.project.ripple, "Ripple"),
                    )
                    .on_hover_text("Ripple (shift following clips when trimming/removing)")
                    .clicked()
                {
                    self.push_undo();
                    self.project.ripple = !self.project.ripple;
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                if ui
                    .add_sized(
                        [60.0, 26.0],
                        egui::Button::new("Export").fill(self.accent()),
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
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Preview").strong().color(WHITE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized([74.0, 20.0], egui::Button::new("Fullscreen").small())
                        .clicked()
                    {
                        self.fullscreen_preview = true;
                    }
                    ui.label(
                        RichText::new(format!(
                            "{}  {}fps  {}x{}",
                            timecode_string(self.playhead_us, self.project.fps),
                            self.project.fps,
                            self.settings.preview_width,
                            self.settings.preview_height
                        ))
                        .monospace()
                        .size(11.0)
                        .color(GRAY),
                    );
                });
            });
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Zoom").size(11.0).color(GRAY));
                if ui.add_sized([30.0, 18.0], egui::Button::new("Fit").small()).clicked() {
                    self.preview_zoom = 1.0;
                }
                ui.add(
                    egui::Slider::new(&mut self.preview_zoom, 0.1..=4.0)
                        .logarithmic(true)
                        .show_value(false),
                );
            });
            ui.add_space(2.0);
            ui.separator();
            let tex = self.preview_texture.clone();
            if let Some(tex) = tex {
                self.paint_preview_image(ui, &tex);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.vertical(|ui| {
                        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                            ui.label(RichText::new("No media in the preview").size(18.0).color(DIM));
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(
                                    "Import media (File ▸ Import media…) and place clips on a timeline lane, or drop media onto a lane directly.",
                                )
                                .size(12.0)
                                .color(GRAY),
                            );
                            ui.add_space(12.0);
                            if ui.button("Import media…").clicked() {
                                self.pick_and_import();
                            }
                            ui.add_space(4.0);
                            if ui.button("Open command palette").clicked() {
                                self.toggle_palette();
                            }
                        });
                    });
                });
            }
        });
    }

    fn paint_preview_image(&mut self, ui: &mut egui::Ui, tex: &egui::TextureHandle) {
        let avail = ui.available_size();
        if avail.x < 8.0 || avail.y < 8.0 {
            return;
        }
        let aspect = self.settings.preview_width as f32 / self.settings.preview_height as f32;
        let mut size = avail;
        if size.x / size.y > aspect {
            size.x = size.y * aspect;
        } else {
            size.y = size.x / aspect;
        }
        size *= self.preview_zoom;
        if size.x < 8.0 || size.y < 8.0 {
            return;
        }
        let center = ui.available_rect_before_wrap().center();
        let img_rect = Rect::from_center_size(center, size);
        ui.painter()
            .rect_filled(egui::Rect::from_center_size(center, size + egui::vec2(12.0, 12.0)), 4.0, TRACK);
        ui.painter()
            .rect_stroke(
                img_rect.shrink(1.0),
                2.0,
                egui::Stroke::new(1.0, DIM),
                StrokeKind::Inside,
            );
        let sized = egui::load::SizedTexture::new(tex.id(), size);
        ui.put(
            img_rect,
            egui::Image::new(sized).fit_to_exact_size(size),
        );
        if self.settings.safe_margins {
            let p = ui.painter();
            let inset = egui::vec2(size.x * 0.05, size.y * 0.05);
            let inner = img_rect.shrink2(inset);
            let guide = egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 50));
            p.rect_stroke(inner, 0.0, guide, StrokeKind::Inside);
            for f in [0.333, 0.667] {
                p.line_segment(
                    [
                        egui::pos2(inner.min.x + inner.width() * f, inner.min.y),
                        egui::pos2(inner.min.x + inner.width() * f, inner.max.y),
                    ],
                    guide,
                );
                p.line_segment(
                    [
                        egui::pos2(inner.min.x, inner.min.y + inner.height() * f),
                        egui::pos2(inner.max.x, inner.min.y + inner.height() * f),
                    ],
                    guide,
                );
            }
        }
    }

    fn fullscreen_preview_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("Preview — fullscreen")
            .id(egui::Id::new("fullscreen_preview"))
            .title_bar(true)
            .resizable(true)
            .default_size(ctx.screen_rect().size() * 0.9)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Exit fullscreen").clicked() {
                        self.fullscreen_preview = false;
                    }
                    ui.add_space(8.0);
                    if ui.button("Fit").clicked() {
                        self.preview_zoom = 1.0;
                    }
                    ui.add(
                        egui::Slider::new(&mut self.preview_zoom, 0.1..=4.0)
                            .logarithmic(true)
                            .show_value(false),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!(
                            "{}  (Esc to exit)",
                            timecode_string(self.playhead_us, self.project.fps)
                        ))
                        .monospace()
                        .size(11.0)
                        .color(GRAY),
                    );
                });
                ui.separator();
                let tex = self.preview_texture.clone();
                if let Some(tex) = tex {
                    self.paint_preview_image(ui, &tex);
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Nothing to preview").color(DIM));
                    });
                }
            });
        if !self.fullscreen_preview {
            self.last_request = None;
        }
    }

    // ── Command palette ────────────────────────────────────────────────────
    fn palette_window(&mut self, ctx: &egui::Context) {
        let commands: [(&str, PaletteAction); 20] = [
            ("Import media…", PaletteAction::Import),
            ("New project", PaletteAction::NewProject),
            ("Open project…", PaletteAction::OpenProject),
            ("Save project", PaletteAction::SaveProject),
            ("Save project as…", PaletteAction::SaveAs),
            ("Export…", PaletteAction::Export),
            ("Add video track", PaletteAction::AddVideoTrack),
            ("Add audio track", PaletteAction::AddAudioTrack),
            ("Add text / subtitles", PaletteAction::AddText),
            ("Insert marker at playhead", PaletteAction::Mark),
            ("Split clip at playhead", PaletteAction::Split),
            ("Delete selection", PaletteAction::DeleteSel),
            ("Toggle loop region", PaletteAction::Loop),
            ("Toggle ripple mode", PaletteAction::Ripple),
            ("Toggle preview fullscreen", PaletteAction::Fullscreen),
            ("Toggle mixer window", PaletteAction::Mixer),
            ("Toggle snap to playhead", PaletteAction::Snap),
            ("Toggle safe margins", PaletteAction::Margins),
            ("Toggle dark mode", PaletteAction::Dark),
            ("Preferences…", PaletteAction::Preferences),
        ];
        egui::Window::new("Command palette")
            .id(egui::Id::new("palette"))
            .fixed_size([420.0, 320.0])
            .collapsible(false)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    let filter = &mut self.palette_filter;
                    ui.add(
                        egui::TextEdit::singleline(filter)
                            .hint_text("Type a command…")
                            .frame(true)
                            .desired_width(f32::INFINITY),
                    );
                    let matched: Vec<usize> = commands
                        .iter()
                        .enumerate()
                        .filter(|(_, (name, _))| {
                            let f = filter.to_lowercase();
                            f.is_empty() || name.to_lowercase().contains(&f)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    if matched.is_empty() {
                        ui.label("No matching commands");
                        return;
                    }
                    if self.palette_sel >= matched.len() {
                        self.palette_sel = 0;
                    }
                    let key = ui.input(|i| {
                        i.key_pressed(egui::Key::ArrowDown) as i32
                            - i.key_pressed(egui::Key::ArrowUp) as i32
                    });
                    if key != 0 {
                        let n = matched.len() as i32;
                        self.palette_sel =
                            (self.palette_sel as i32 + key + n) as usize % matched.len();
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("palette_list")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for (i, &ci) in matched.iter().enumerate() {
                                let (name, _act) = commands[ci];
                                let selected = i == self.palette_sel;
                                if ui
                                    .selectable_label(selected, RichText::new(name).size(13.0))
                                    .clicked()
                                {
                                    self.run_palette(ctx, commands[ci].1);
                                    return;
                                }
                            }
                        });
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if enter && !matched.is_empty() {
                        self.run_palette(ctx, commands[matched[self.palette_sel]].1);
                        return;
                    }
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if esc {
                        self.palette_open = false;
                    }
                });
            });
    }

    fn run_palette(&mut self, ctx: &egui::Context, a: PaletteAction) {
        self.palette_open = false;
        self.palette_filter.clear();
        self.palette_sel = 0;
        match a {
            PaletteAction::Import => self.pick_and_import(),
            PaletteAction::NewProject => self.new_project(),
            PaletteAction::OpenProject => self.prompt_open_project(),
            PaletteAction::SaveProject => self.save_project(),
            PaletteAction::SaveAs => self.save_project_as(),
            PaletteAction::Export => self.export_open = true,
            PaletteAction::AddVideoTrack => self.add_track(0),
            PaletteAction::AddAudioTrack => self.add_track(1),
            PaletteAction::AddText => {
                let n = self.project.text_tracks.len();
                self.project.text_tracks.push(crate::timeline::Track {
                    name: format!("Text {}", n + 1),
                    clips: Vec::new(),
                    ..crate::timeline::Track::default()
                });
                self.push_undo();
            }
            PaletteAction::Mark => {
                let name = format!("Marker {}", self.project.markers.len() + 1);
                self.project.markers.push(crate::timeline::Marker {
                    time_us: self.playhead_us.max(0),
                    name,
                    color: [240, 180, 60],
                });
                self.push_undo();
            }
            PaletteAction::Split => self.split_selected(),
            PaletteAction::DeleteSel => self.delete_selected(),
            PaletteAction::Loop => {
                if self.project.loop_start.is_some() && self.project.loop_end.is_some() {
                    self.project.loop_start = None;
                    self.project.loop_end = None;
                } else {
                    let a = (self.playhead_us - SECOND_US).max(0);
                    let b = (self.playhead_us + SECOND_US).max(1);
                    self.project.loop_start = Some(a);
                    self.project.loop_end = Some(b);
                }
            }
            PaletteAction::Ripple => self.project.ripple = !self.project.ripple,
            PaletteAction::Fullscreen => self.fullscreen_preview = !self.fullscreen_preview,
            PaletteAction::Mixer => self.mixer_open = !self.mixer_open,
            PaletteAction::Snap => {
                self.settings.snap_to_playhead = !self.settings.snap_to_playhead;
            }
            PaletteAction::Margins => self.settings.safe_margins = !self.settings.safe_margins,
            PaletteAction::Dark => {
                self.settings.dark_mode = !self.settings.dark_mode;
                apply_theme(ctx, self.settings.dark_mode, self.settings.accent);
            }
            PaletteAction::Preferences => self.settings_open = true,
        }
    }

    // ── Mixer ──────────────────────────────────────────────────────────────
    fn mixer_window(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        egui::Window::new("Mixer")
            .id(egui::Id::new("mixer"))
            .resizable(true)
            .default_size([340.0, 420.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Master").strong().color(WHITE));
                    ui.add(
                        egui::Slider::new(&mut self.project.master_gain, -60.0..=6.0)
                            .text("dB")
                            .suffix(""),
                    );
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let gains: Vec<(usize, String)> = self
                        .project
                        .audio_tracks
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (i, t.name.clone()))
                        .collect();
                    for (i, name) in gains {
                        ui.push_id(("mix", i), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&name).size(12.0).color(GRAY));
                                let mut muted = self.project.audio_tracks[i].muted;
                                let mut solo = self.project.audio_tracks[i].solo;
                                if ui.selectable_label(muted, "M").clicked() {
                                    muted = !muted;
                                    changed = true;
                                }
                                if ui.selectable_label(solo, "S").clicked() {
                                    solo = !solo;
                                    changed = true;
                                }
                                if changed {
                                    self.project.audio_tracks[i].muted = muted;
                                    self.project.audio_tracks[i].solo = solo;
                                }
                            });
                            let db = self.project.audio_tracks[i].gain_db;
                            let mut db = db;
                            if ui
                                .add(egui::Slider::new(&mut db, -60.0..=12.0).text("dB"))
                                .changed()
                            {
                                self.project.audio_tracks[i].gain_db = db;
                                changed = true;
                            }
                        });
                    }
                });
            });
        if changed {
            self.dirty = true;
            self.audio.refresh(&self.project);
        }
    }

    // ── Relink media ───────────────────────────────────────────────────────
    fn relink_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("Relink media")
            .id(egui::Id::new("relink"))
            .resizable(true)
            .default_size([520.0, 360.0])
            .show(ctx, |ui| {
                let missing: Vec<AssetId> = self
                    .project
                    .assets
                    .assets
                    .iter()
                    .filter(|(_, a)| a.path.is_empty() || !std::path::Path::new(&a.path).exists())
                    .map(|(id, _)| *id)
                    .collect();
                if missing.is_empty() {
                    ui.label("All media files are reachable.");
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for id in missing {
                        let asset = self.project.assets.assets.get(&id).cloned();
                        let Some(asset) = asset else {
                            continue;
                        };
                        let a_name = asset.name.clone();
                        let a_path = asset.path.clone();
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&a_name).size(12.0).color(WHITE));
                                ui.label(RichText::new(&a_path).size(10.0).color(DIM));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Relink…").clicked() {
                                    if let Some(f) = rfd::FileDialog::new()
                                        .add_filter("Media", &["mp4", "mov", "mkv", "webm", "m4v", "mp3", "wav", "ogg", "flac", "jpg", "jpeg", "png"])
                                        .pick_file()
                                    {
                                        let p = f.to_string_lossy().to_string();
                                        let is_video = matches!(
                                            asset.kind,
                                            crate::timeline::AssetKind::Video
                                        );
                                        if let Some(a) = self.project.assets.assets.get_mut(&id) {
                                            a.path = p.clone();
                                            a.name = std::path::Path::new(&p)
                                                .file_name()
                                                .map(|s| s.to_string_lossy().to_string())
                                                .unwrap_or_else(|| a_name.clone());
                                            a.peaks.clear();
                                            a.pcm = None;
                                            if is_video {
                                                a.filmstrip = None;
                                            }
                                        }
                                        self.toasts.push(
                                            ("Asset relinked".into(), std::time::Instant::now()),
                                        );
                                        self.dirty = true;
                                    }
                                }
                            });
                        });
                        ui.separator();
                    }
                });
            });
    }

    // ── Timeline (bottom) ────────────────────────────────────────────────────
    fn timeline(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(240.0)
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
                    ui.add_space(6.0);
                    if ui.small_button("−").clicked() {
                        self.timeline_zoom = (self.timeline_zoom * 0.85).max(4.0);
                    }
                    if ui.small_button("+").clicked() {
                        self.timeline_zoom = (self.timeline_zoom * 1.18).min(3000.0);
                    }
                    if ui.small_button("Fit").clicked() {
                        let dur = self.project.duration_us().max(SECOND_US);
                        let view_w = (ui.available_width() - 190.0).max(100.0);
                        self.timeline_zoom =
                            (view_w * SECOND_US as f32 / dur as f32).clamp(4.0, 3000.0);
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
                ui.add_space(2.0);

                let left_w = 150.0;
                let ruler_h = 20.0;
                let minimap_h = 16.0;
                let row_h = 38.0;
                let frame_us = (SECOND_US as f64 / self.project.fps.max(1.0)) as i64;
                let dur_us = self.project.duration_us().max(SECOND_US);
                let vid_rows = self.project.video_tracks.len().max(1);
                let aud_rows = self.project.audio_tracks.len().max(1);
                let txt_rows = self.project.text_tracks.len().max(1);
                let total_rows = (vid_rows + aud_rows + txt_rows) as f32;
                let total_h = minimap_h + ruler_h + total_rows * row_h + 14.0;
                let avail_h = ui.available_height().max(total_h + 10.0);

                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width().max(320.0), avail_h),
                    Sense::click_and_drag(),
                );
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, PANEL);

                // Geometry helpers for time <-> px.
                let content_min_x = rect.min.x + left_w;
                let view_w = rect.width() - left_w;
                let px_per_sec = self.timeline_zoom;
                let scroll_us0 = self.timeline_scroll_us;
                let total_px = dur_us as f32 / SECOND_US as f32 * px_per_sec;
                let max_scroll = (total_px - view_w).max(0.0);
                let scroll_px = ((scroll_us0 as f32 / SECOND_US as f32) * px_per_sec)
                    .clamp(0.0, max_scroll);
                let x_for = |t: i64| -> f32 {
                    content_min_x + (t as f32 / SECOND_US as f32) * px_per_sec - scroll_px
                };
                let t_for = |px: f32| -> i64 {
                    (scroll_us0 as f32
                        + ((px - content_min_x + scroll_px) / px_per_sec) * SECOND_US as f32)
                        as i64
                };

                // --- Zoom (Ctrl/Shift + wheel) and horizontal scroll (wheel) ---
                if let Some(p) = ctx.input(|i| i.pointer.hover_pos()) {
                    if rect.contains(p) && p.x > content_min_x {
                        let (wheel, ctrl, shift) = ctx.input(|i| {
                            (i.smooth_scroll_delta.y, i.modifiers.ctrl, i.modifiers.shift)
                        });
                        if wheel != 0.0 {
                            if ctrl || shift {
                                // Zoom keeping the time under the cursor stable.
                                let cursor_t = t_for(p.x);
                                let cursor_dx = p.x - content_min_x + scroll_px;
                                let nz = (px_per_sec * (1.0 + wheel * 0.06))
                                    .clamp(4.0, 3000.0);
                                let scroll_us_new = (cursor_t as f32 / SECOND_US as f32) * nz
                                    - cursor_dx / nz * SECOND_US as f32;
                                self.timeline_zoom = nz;
                                let max_s = (dur_us as f32 / SECOND_US as f32 * nz - view_w)
                                    .max(0.0);
                                self.timeline_scroll_us =
                                    scroll_us_new.max(0.0).min(max_s) as i64;
                            } else if rect.contains(p) {
                                let dr = wheel * 60.0 * px_per_sec * 0.05;
                                self.timeline_scroll_us =
                                    ((self.timeline_scroll_us as f32 + dr).max(0.0)) as i64;
                            }
                        }
                    }
                }

                // --- Track lanes (with headers) ---
                let mut lane_rects: Vec<(u8, usize, Rect)> = Vec::new();
                let mut clips: Vec<(u8, usize, usize, Rect, (i64, i64))> = Vec::new();

                // Video lanes.
                for (ti, tr) in self.project.video_tracks.iter().enumerate() {
                    let y0 = rect.min.y + minimap_h + ruler_h + ti as f32 * row_h;
                    let header = Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(content_min_x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(header, 2.0, TRACK);
                    let lane = Rect::from_min_max(
                        egui::pos2(content_min_x, y0),
                        egui::pos2(rect.max.x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.rect_filled(
                        Rect::from_min_size(lane.min, egui::vec2(2.0, lane.height())),
                        0.0,
                        Color32::from_rgb(tr.color[0], tr.color[1], tr.color[2]),
                    );
                    // Header text (muted tracks dimmed).
                    let name_col = if tr.muted {
                        Color32::from_rgb(110, 110, 118)
                    } else {
                        Color32::from_rgb(215, 215, 225)
                    };
                    painter.text(
                        header.min + egui::vec2(8.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        if tr.muted { format!("{} 🔇", tr.name) } else { tr.name.clone() },
                        egui::FontId::proportional(11.0),
                        name_col,
                    );
                    lane_rects.push((0, ti, lane));
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw = c.on_timeline_us() as f32 / SECOND_US as f32 * px_per_sec;
                        let cx = x_for(c.timeline_start);
                        if cx + cw < content_min_x || cx > rect.max.x {
                            continue;
                        }
                        let cr = Rect::from_min_max(
                            egui::pos2(cx + 1.0, y0 + 3.0),
                            egui::pos2(cx + cw - 1.0, y0 + row_h - 7.0),
                        );
                        painter.rect_filled(cr, 3.0, VIDEO_CLIP);
                        clips.push((0, ti, ci, cr, (c.timeline_start, c.on_timeline_us())));
                    }
                }

                // Audio lanes.
                for (ai, tr) in self.project.audio_tracks.iter().enumerate() {
                    let y0 = rect.min.y + minimap_h + ruler_h + (vid_rows + ai) as f32 * row_h;
                    let header = Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(content_min_x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(header, 2.0, TRACK);
                    let lane = Rect::from_min_max(
                        egui::pos2(content_min_x, y0),
                        egui::pos2(rect.max.x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.rect_filled(
                        Rect::from_min_size(lane.min, egui::vec2(2.0, lane.height())),
                        0.0,
                        Color32::from_rgb(tr.color[0], tr.color[1], tr.color[2]),
                    );
                    let name_col = if tr.muted {
                        Color32::from_rgb(110, 110, 118)
                    } else {
                        Color32::from_rgb(215, 215, 225)
                    };
                    painter.text(
                        header.min + egui::vec2(8.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        if tr.muted { format!("{} 🔇", tr.name) } else { tr.name.clone() },
                        egui::FontId::proportional(11.0),
                        name_col,
                    );
                    lane_rects.push((1, ai, lane));
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw = c.on_timeline_us() as f32 / SECOND_US as f32 * px_per_sec;
                        let cx = x_for(c.timeline_start);
                        if cx + cw < content_min_x || cx > rect.max.x {
                            continue;
                        }
                        let cr = Rect::from_min_max(
                            egui::pos2(cx + 1.0, y0 + 3.0),
                            egui::pos2(cx + cw - 1.0, y0 + row_h - 7.0),
                        );
                        painter.rect_filled(cr, 3.0, AUDIO_CLIP);
                        clips.push((1, ai, ci, cr, (c.timeline_start, c.on_timeline_us())));
                    }
                }

                // Text lanes.
                for (ti, tr) in self.project.text_tracks.iter().enumerate() {
                    let y0 = rect.min.y
                        + minimap_h
                        + ruler_h
                        + (vid_rows + aud_rows + ti) as f32 * row_h;
                    let header = Rect::from_min_max(
                        egui::pos2(rect.min.x, y0),
                        egui::pos2(content_min_x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(header, 2.0, TRACK);
                    let lane = Rect::from_min_max(
                        egui::pos2(content_min_x, y0),
                        egui::pos2(rect.max.x, y0 + row_h - 3.0),
                    );
                    painter.rect_filled(lane, 2.0, TRACK);
                    painter.rect_filled(
                        Rect::from_min_size(lane.min, egui::vec2(2.0, lane.height())),
                        0.0,
                        Color32::from_rgb(tr.color[0], tr.color[1], tr.color[2]),
                    );
                    painter.text(
                        header.min + egui::vec2(8.0, (row_h - 12.0) / 2.0),
                        egui::Align2::LEFT_CENTER,
                        tr.name.clone(),
                        egui::FontId::proportional(11.0),
                        Color32::from_rgb(215, 215, 225),
                    );
                    lane_rects.push((2, ti, lane));
                    for (ci, c) in tr.clips.iter().enumerate() {
                        let cw =
                            (c.timeline_end - c.timeline_start) as f32 / SECOND_US as f32 * px_per_sec;
                        let cx = x_for(c.timeline_start);
                        if cx + cw < content_min_x || cx > rect.max.x {
                            continue;
                        }
                        let cr = Rect::from_min_max(
                            egui::pos2(cx + 1.0, y0 + 3.0),
                            egui::pos2(cx + cw - 1.0, y0 + row_h - 7.0),
                        );
                        painter.rect_filled(cr, 3.0, TEXT_CLIP);
                        clips.push((
                            2,
                            ti,
                            ci,
                            cr,
                            (c.timeline_start, c.timeline_end - c.timeline_start),
                        ));
                    }
                }

                // --- Track header buttons (Mute / Solo / Lock) via ui.interact ---
                let mut header_click: Option<(u8, usize, u8)> = None; // (kind, idx, btn 0=m,1=s,2=l)
                let btn_sz = 16.0f32;
                for lane in &lane_rects {
                    let (kind, idx, lane_rect) = *lane;
                    let (muted, solo, locked) = match kind {
                        0 => {
                            let tr = &self.project.video_tracks[idx];
                            (tr.muted, tr.solo, tr.locked)
                        }
                        1 => {
                            let tr = &self.project.audio_tracks[idx];
                            (tr.muted, tr.solo, tr.locked)
                        }
                        _ => {
                            let tr = &self.project.text_tracks[idx];
                            (tr.muted, tr.solo, tr.locked)
                        }
                    };
                    let mut bx = lane_rect.min.x - 56.0;
                    let mut b = 0;
                    while b < 3 {
                        let label = ["M", "S", "L"][b];
                        let kind = kind;
                        if kind == 2 && b == 1 {
                            b += 1;
                            continue;
                        }
                        if b == 0 && (lane_rect.min.x - (bx + btn_sz)) < 2.0 {
                            break;
                        }
                        let active = match b {
                            0 => muted,
                            1 => solo,
                            _ => locked,
                        };
                        let br = Rect::from_min_size(
                            egui::pos2(bx, lane_rect.center().y - btn_sz / 2.0),
                            egui::vec2(btn_sz, btn_sz),
                        );
                        let id = ui.id().with(("t", kind, idx, b));
                        let r = ui.interact(br, id, Sense::click());
                        let bg = if active {
                            self.accent()
                        } else {
                            Color32::from_rgb(55, 55, 63)
                        };
                        painter.rect_filled(br, 2.0, bg);
                        if r.hovered() {
                            painter.rect_stroke(
                                br,
                                2.0,
                                egui::Stroke::new(1.0, WHITE),
                                StrokeKind::Inside,
                            );
                        }
                        painter.text(
                            br.center(),
                            egui::Align2::CENTER_CENTER,
                            label,
                            egui::FontId::monospace(9.0),
                            WHITE,
                        );
                        if r.clicked() {
                            header_click = Some((kind, idx, b as u8));
                        }
                        b += 1;
                        bx -= btn_sz + 3.0;
                    }
                }
                if let Some((kind, idx, b)) = header_click {
                    self.push_undo();
                    match b {
                        0 => {
                            if kind == 0 {
                                self.project.video_tracks[idx].muted = !self.project.video_tracks[idx].muted;
                            } else if kind == 1 {
                                self.project.audio_tracks[idx].muted = !self.project.audio_tracks[idx].muted;
                                self.audio.refresh(&self.project);
                            }
                        }
                        1 => {
                            if kind == 1 {
                                self.project.audio_tracks[idx].solo = !self.project.audio_tracks[idx].solo;
                                self.audio.refresh(&self.project);
                            }
                        }
                        _ => {
                            if kind == 0 {
                                self.project.video_tracks[idx].locked = !self.project.video_tracks[idx].locked;
                            } else if kind == 1 {
                                self.project.audio_tracks[idx].locked = !self.project.audio_tracks[idx].locked;
                            } else {
                                self.project.text_tracks[idx].locked = !self.project.text_tracks[idx].locked;
                            }
                        }
                    }
                }

                // --- Timeline drag (move / trim) ---
                if self.timeline_drag.is_some() {
                    let release = ctx.input(|i| i.pointer.primary_released());
                    let p = ctx.input(|i| i.pointer.interact_pos());
                    if let Some(p) = p {
                        if rect.contains(p) && !release {
                            let t = t_for(p.x).max(0);
                            match self.timeline_drag {
                                Some(TimelineDrag::Move { kind, track, clip, grab_us }) => {
                                    let new_start = (t - grab_us).max(0);
                                    let snapped = self.snapped_time(new_start);
                                    match kind {
                                        0 => {
                                            if let Some(c) = self
                                                .project
                                                .video_tracks
                                                .get_mut(track)
                                                .and_then(|tr| tr.clips.get_mut(clip))
                                            {
                                                c.timeline_start = snapped;
                                            }
                                        }
                                        1 => {
                                            if let Some(c) = self
                                                .project
                                                .audio_tracks
                                                .get_mut(track)
                                                .and_then(|tr| tr.clips.get_mut(clip))
                                            {
                                                c.timeline_start = snapped;
                                            }
                                        }
                                        _ => {
                                            if let Some(c) = self
                                                .project
                                                .text_tracks
                                                .get_mut(track)
                                                .and_then(|tr| tr.clips.get_mut(clip))
                                            {
                                                let len = c.timeline_end - c.timeline_start;
                                                c.timeline_start = snapped;
                                                c.timeline_end = c.timeline_start + len;
                                            }
                                        }
                                    }
                                    self.dirty = true;
                                }
                                Some(TimelineDrag::TrimL { kind, track, clip }) => {
                                    match kind {
                                        0 => {
                                            if let Some(c) = self
                                                .project
                                                .video_tracks
                                                .get_mut(track)
                                                .and_then(|tr| tr.clips.get_mut(clip))
                                            {
                                                let s_in = c.source_at((t - c.timeline_start).max(0));
                                                c.source_in = s_in.clamp(0, c.source_out - frame_us.max(1));
                                            }
                                        }
                                        1 => {
                                            if let Some(c) = self
                                                .project
                                                .audio_tracks
                                                .get_mut(track)
                                                .and_then(|tr| tr.clips.get_mut(clip))
                                            {
                                                let s_in = c.source_at((t - c.timeline_start).max(0));
                                                c.source_in = s_in.clamp(0, c.source_out - frame_us.max(1));
                                            }
                                        }
                                        _ => {
                                            if let Some(c) = self
                                                .project
                                                .text_tracks
                                                .get_mut(track)
                                                .and_then(|tr| tr.clips.get_mut(clip))
                                            {
                                                let s = t.max(c.timeline_start + frame_us);
                                                let s = s.min(c.timeline_end - frame_us.max(1));
                                                c.timeline_start = s.max(0);
                                                if c.timeline_end <= c.timeline_start {
                                                    c.timeline_end = c.timeline_start + frame_us;
                                                }
                                            }
                                        }
                                    }
                                    self.dirty = true;
                                }
                                Some(TimelineDrag::TrimR { kind, track, clip }) => {
                                    let info: Option<AudioClip> = match kind {
                                        0 => self
                                            .project
                                            .video_tracks
                                            .get(track)
                                            .and_then(|tr| tr.clips.get(clip))
                                            .cloned()
                                            .map(|c| AudioClip {
                                                asset: c.asset,
                                                source_in: c.source_in,
                                                source_out: c.source_out,
                                                timeline_start: c.timeline_start,
                                                ..AudioClip::default()
                                            }),
                                        1 => self
                                            .project
                                            .audio_tracks
                                            .get(track)
                                            .and_then(|tr| tr.clips.get(clip))
                                            .cloned(),
                                        _ => None,
                                    };
                                    if let Some(info) = info {
                                        let max_out = self
                                            .project
                                            .assets
                                            .get(info.asset)
                                            .map(|a| a.duration_us)
                                            .unwrap_or(info.source_out);
                                        let local = (t - info.timeline_start).max(frame_us);
                                        let usable =
                                            (info.on_timeline_us() - frame_us.max(1)).max(1);
                                        let s_out = info
                                            .source_at(local.min(usable))
                                            .clamp(info.source_in + frame_us.max(1), max_out);
                                        if kind == 0 {
                                            if let Some(c) = self
                                                .project
                                                .video_tracks
                                                .get_mut(track)
                                                .and_then(|tr| tr.clips.get_mut(clip))
                                            {
                                                c.source_out = s_out;
                                            }
                                        } else if let Some(c) = self
                                            .project
                                            .audio_tracks
                                            .get_mut(track)
                                            .and_then(|tr| tr.clips.get_mut(clip))
                                        {
                                            c.source_out = s_out;
                                        }
                                    }
                                    if kind == 2 {
                                        if let Some(c) = self
                                            .project
                                            .text_tracks
                                            .get_mut(track)
                                            .and_then(|tr| tr.clips.get_mut(clip))
                                        {
                                            let s = t.max(c.timeline_start + frame_us);
                                            c.timeline_end = s;
                                        }
                                    }
                                    self.dirty = true;
                                }
                                _ => {}
                            }
                        }
                    }
                    if release {
                        self.timeline_drag = None;
                        self.audio.refresh(&self.project);
                        self.last_request = None;
                    }
                } else if response.dragged() {
                    // Begin a drag when the pointer is over a clip (move) or a
                    // trim handle.
                    if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                        if rect.contains(p) {
                            for (kind, track, clip, cr, _) in clips.iter().rev() {
                                let (kind, track, clip, cr) = (*kind, *track, *clip, *cr);
                                let hover_x = p.x;
                                if cr.contains(p) {
                                    if hover_x <= cr.min.x + 6.0 {
                                        self.push_undo();
                                        self.timeline_drag = Some(TimelineDrag::TrimL {
                                            kind,
                                            track,
                                            clip,
                                        });
                                    } else if hover_x >= cr.max.x - 6.0 {
                                        self.push_undo();
                                        self.timeline_drag = Some(TimelineDrag::TrimR {
                                            kind,
                                            track,
                                            clip,
                                        });
                                    } else {
                                        self.push_undo();
                                        self.timeline_drag = Some(TimelineDrag::Move {
                                            kind,
                                            track,
                                            clip,
                                            grab_us: (t_for(p.x) - clip_time_start(
                                                &self.project, kind, track, clip,
                                            ))
                                            .max(0),
                                        });
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }

                // --- Waveforms + clip labels + trim handles + selection frames ----
                for (kind, track, clip, cr, _) in &clips {
                    let (kind, track, clip, cr) = (*kind, *track, *clip, *cr);
                    match kind {
                        1 => {
                            // Audio waveform scaled by source position.
                            if let Some(c) = self
                                .project
                                .audio_tracks
                                .get(track)
                                .and_then(|tr| tr.clips.get(clip))
                            {
                                if self.settings.show_peaks {
                                    if let Some(asset) = self.project.assets.get(c.asset) {
                                        if !asset.peaks.is_empty() {
                                            let n = asset.peaks.len();
                                            let dur = asset.duration_us.max(1) as f32;
                                            let mut px = cr.min.x;
                                            let mid = cr.center().y;
                                            while px < cr.max.x {
                                                let local =
                                                    (px - cr.min.x) / cr.width();
                                                let src =
                                                    c.source_at((local * c.on_timeline_us() as f32) as i64)
                                                        as f32;
                                                let bi = ((src / dur * n as f32) as usize).min(n - 1);
                                                let (lo, hi) = asset.peaks[bi];
                                                painter.line_segment(
                                                    [
                                                        egui::pos2(px, mid - hi * cr.height() * 0.5),
                                                        egui::pos2(px, mid - lo * cr.height() * 0.5),
                                                    ],
                                                    egui::Stroke::new(1.0, WAVE),
                                                );
                                                px += 1.0;
                                            }
                                        }
                                    }
                                }
                                painter.text(
                                    cr.min + egui::vec2(3.0, 1.0),
                                    egui::Align2::LEFT_TOP,
                                    &c_name(&self.project, c.asset),
                                    egui::FontId::proportional(9.0),
                                    Color32::from_rgb(235, 235, 240),
                                );
                            }
                        }
                        0 => {
                            if let Some(c) = self
                                .project
                                .video_tracks
                                .get(track)
                                .and_then(|tr| tr.clips.get(clip))
                            {
                                painter.text(
                                    cr.min + egui::vec2(3.0, 2.0),
                                    egui::Align2::LEFT_TOP,
                                    &c_name(&self.project, c.asset),
                                    egui::FontId::proportional(10.0),
                                    WHITE,
                                );
                                // Speed badge.
                                if (c.speed - 1.0).abs() > 0.001 {
                                    painter.text(
                                        cr.max - egui::vec2(3.0, 2.0),
                                        egui::Align2::RIGHT_BOTTOM,
                                        format!("{:.2}×", c.speed),
                                        egui::FontId::proportional(9.0),
                                        Color32::from_rgb(255, 220, 120),
                                    );
                                }
                                if c.transition_us > 0 {
                                    painter.text(
                                        cr.min + egui::vec2(3.0, cr.height() - 13.0),
                                        egui::Align2::LEFT_TOP,
                                        "⊳",
                                        egui::FontId::proportional(11.0),
                                        Color32::from_rgb(150, 220, 255),
                                    );
                                }
                            }
                        }
                        _ => {
                            if let Some(c) = self
                                .project
                                .text_tracks
                                .get(track)
                                .and_then(|tr| tr.clips.get(clip))
                            {
                                let label = c.text.replace('\n', " ").trim().to_string();
                                painter.text(
                                    cr.center(),
                                    egui::Align2::CENTER_CENTER,
                                    &label,
                                    egui::FontId::proportional(10.0),
                                    Color32::from_rgb(245, 235, 200),
                                );
                            }
                        }
                    }
                    // Trim handles.
                    painter.rect_filled(
                        Rect::from_min_size(
                            cr.min,
                            egui::vec2((cr.width() * 0.04).clamp(2.0, 7.0), cr.height()),
                        ),
                        2.0,
                        Color32::from_rgba_unmultiplied(0, 0, 0, 90),
                    );
                    painter.rect_filled(
                        Rect::from_min_size(
                            egui::pos2(cr.max.x - (cr.width() * 0.04).clamp(2.0, 7.0), cr.min.y),
                            egui::vec2((cr.width() * 0.04).clamp(2.0, 7.0), cr.height()),
                        ),
                        2.0,
                        Color32::from_rgba_unmultiplied(0, 0, 0, 90),
                    );

                    // Selection indicators.
                    let is_sel = matches!(
                        self.selected,
                        Selection::Video { track: t, clip: c }
                            if t == track && c == clip
                    ) || matches!(
                        self.selected,
                        Selection::Audio { track: t, clip: c }
                            if t == track && c == clip
                    ) || matches!(
                        self.selected,
                        Selection::Text { track: t, clip: c }
                            if t == track && c == clip
                    );
                    let in_multi = self.multi_selection.iter().any(|m| {
                        matches!(
                            m,
                            Selection::Video { track: t, clip: c }
                                if *t == track && *c == clip
                        ) || matches!(
                            m,
                            Selection::Audio { track: t, clip: c }
                                if *t == track && *c == clip
                        ) || matches!(
                            m,
                            Selection::Text { track: t, clip: c }
                                if *t == track && *c == clip
                        )
                    });
                    if is_sel || in_multi {
                        painter.rect_stroke(
                            cr,
                            3.0,
                            egui::Stroke::new(1.5, self.accent()),
                            StrokeKind::Inside,
                        );
                    }
                }

                // --- Click handling (select, ruler seek) ---
                if response.clicked() {
                    if let Some(p) = response.interact_pointer_pos() {
                        if p.x > content_min_x {
                            let hit = clips
                                .iter()
                                .rev()
                                .find(|(_, _, _, cr, _)| cr.contains(p));
                            match hit {
                                Some((kind, track, clip, _, _)) => {
                                    let sel = match *kind {
                                        0 => Selection::Video { track: *track, clip: *clip },
                                        1 => Selection::Audio { track: *track, clip: *clip },
                                        _ => Selection::Text { track: *track, clip: *clip },
                                    };
                                    let ctrl = ctx.input(|i| i.modifiers.ctrl);
                                    if ctrl {
                                        if self.multi_selection.iter().any(|m| {
                                            std::mem::discriminant(m) == std::mem::discriminant(&sel)
                                                && selection_equals(*m, sel)
                                        }) {
                                            self.multi_selection
                                                .retain(|m| !selection_equals(*m, sel));
                                        } else {
                                            self.multi_selection.push(sel);
                                        }
                                        self.selected = sel;
                                    } else {
                                        self.multi_selection.clear();
                                        self.selected = sel;
                                    }
                                }
                                None => {
                                    self.multi_selection.clear();
                                    self.selected = Selection::None;
                                    // Seek when clicking in the ruler band.
                                    if p.y < rect.min.y + minimap_h + ruler_h {
                                        self.playhead_us = t_for(p.x).clamp(0, dur_us);
                                        self.seek_marker();
                                    }
                                }
                            }
                        }
                    }
                }

                // --- Minimap (whole-project overview) ---
                {
                    let mm = Rect::from_min_max(
                        egui::pos2(rect.min.x, rect.min.y),
                        egui::pos2(rect.max.x, rect.min.y + minimap_h),
                    );
                    painter.rect_filled(mm, 2.0, Color32::from_rgb(16, 16, 20));
                    for (kind, _ti, _ci, _cr, span) in &clips {
                        let (s, d) = *span;
                        let x0 = mm.min.x + s as f32 / dur_us as f32 * mm.width();
                        let w = (d as f32 / dur_us as f32 * mm.width()).max(1.5);
                        let col = match kind {
                            0 => VIDEO_CLIP,
                            1 => AUDIO_CLIP,
                            _ => TEXT_CLIP,
                        };
                        painter.rect_filled(
                            Rect::from_min_size(egui::pos2(x0, mm.min.y + 2.0), egui::vec2(w, mm.height() - 4.0)),
                            1.0,
                            col,
                        );
                    }
                    // Viewport indicator.
                    let vx0 = mm.min.x + self.timeline_scroll_us as f32 / dur_us as f32 * mm.width();
                    let vw = (view_w / total_px.max(1.0) * mm.width()).min(mm.width());
                    painter.rect_stroke(
                        Rect::from_min_size(
                            egui::pos2(vx0, mm.min.y),
                            egui::vec2(vw, mm.height()),
                        ),
                        1.0,
                        egui::Stroke::new(1.0, Color32::from_rgb(120, 130, 150)),
                        StrokeKind::Inside,
                    );
                    // Click on minimap jumps.
                    let mm_id = ui.id().with("minimap");
                    let mm_r = ui.interact(mm, mm_id, Sense::click_and_drag());
                    if mm_r.dragged() || mm_r.clicked() {
                        if let Some(p) = mm_r.interact_pointer_pos() {
                            let frac = ((p.x - mm.min.x) / mm.width()).clamp(0.0, 1.0);
                            let target =
                                (frac * dur_us as f32) as i64 - (view_w / px_per_sec) as i64 / 2;
                            self.timeline_scroll_us =
                                (target.max(0) as f32).min(max_scroll) as i64;
                        }
                    }
                }

                // --- Ruler, markers, loop region ---
                {
                    // Loop region shading on lanes area.
                    if let (Some(ls), Some(le)) =
                        (self.project.loop_start, self.project.loop_end)
                    {
                        if le > ls {
                            let lx0 = x_for(ls).max(content_min_x);
                            let lx1 = x_for(le).min(rect.max.x);
                            if lx1 > lx0 {
                                painter.rect_filled(
                                    Rect::from_min_max(
                                        egui::pos2(lx0, rect.min.y + minimap_h),
                                        egui::pos2(lx1, rect.max.y),
                                    ),
                                    0.0,
                                    Color32::from_rgba_unmultiplied(120, 190, 255, 12),
                                );
                            }
                        }
                    }
                    // Ruler ticks.
                    let ruler_y = rect.min.y + minimap_h;
                    let major_us = {
                        // Choose a nice tick interval.
                        let sec = px_per_sec;
                        let mut candidates = [0.25f64, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0];
                        candidates.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let mut pick = 10.0;
                        for c in candidates {
                            if c as f32 * sec >= 70.0 {
                                pick = c;
                                break;
                            }
                        }
                        (pick * SECOND_US as f64) as i64
                    };
                    let t0 = t_for(rect.min.x - 1.0).max(0);
                    let t1 = t_for(rect.max.x);
                    let mut tick = (t0 / major_us) * major_us;
                    painter.rect_filled(
                        Rect::from_min_max(
                            egui::pos2(rect.min.x, ruler_y),
                            egui::pos2(rect.max.x, ruler_y + ruler_h),
                        ),
                        0.0,
                        Color32::from_rgb(13, 13, 16),
                    );
                    while tick <= t1 {
                        let tx = x_for(tick);
                        if tx >= content_min_x {
                            painter.line_segment(
                                [
                                    egui::pos2(tx, ruler_y),
                                    egui::pos2(tx, ruler_y + 6.0),
                                ],
                                egui::Stroke::new(1.0, Color32::from_rgb(150, 150, 160)),
                            );
                            painter.text(
                                egui::pos2(tx + 2.0, ruler_y + 2.0),
                                egui::Align2::LEFT_TOP,
                                timecode_string(tick, self.project.fps),
                                egui::FontId::proportional(9.0),
                                Color32::from_rgb(150, 150, 160),
                            );
                        }
                        tick += major_us;
                    }
                    // Loop endpoints.
                    if let (Some(ls), Some(le)) =
                        (self.project.loop_start, self.project.loop_end)
                    {
                        if le > ls {
                            for (t, l) in [(ls, "◫"), (le, "◭")] {
                                let lx = x_for(t).clamp(content_min_x, rect.max.x);
                                painter.text(
                                    egui::pos2(lx, ruler_y + ruler_h - 4.0),
                                    egui::Align2::CENTER_CENTER,
                                    l,
                                    egui::FontId::proportional(10.0),
                                    Color32::from_rgb(120, 190, 255),
                                );
                            }
                        }
                    }
                    // Markers.
                    for m in &self.project.markers {
                        let mx = x_for(m.time_us);
                        if mx < content_min_x - 6.0 || mx > rect.max.x {
                            continue;
                        }
                        let col = Color32::from_rgb(m.color[0], m.color[1], m.color[2]);
                        painter.add(egui::Shape::convex_polygon(
                            vec![
                                egui::pos2(mx, ruler_y + ruler_h - 9.0),
                                egui::pos2(mx - 4.0, ruler_y + ruler_h),
                                egui::pos2(mx + 4.0, ruler_y + ruler_h),
                            ],
                            col,
                            egui::Stroke::NONE,
                        ));
                        if !m.name.is_empty() {
                            painter.text(
                                egui::pos2(mx + 5.0, ruler_y + ruler_h - 9.0),
                                egui::Align2::LEFT_TOP,
                                &m.name,
                                egui::FontId::proportional(9.0),
                                col,
                            );
                        }
                    }
                }

                // --- Playhead ---
                let px = x_for(self.playhead_us).clamp(content_min_x, rect.max.x);
                painter.line_segment(
                    [egui::pos2(px, rect.min.y + minimap_h + ruler_h), egui::pos2(px, rect.max.y)],
                    egui::Stroke::new(2.0, PLAYHEAD),
                );
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(px, rect.min.y + minimap_h),
                        egui::pos2(px - 5.0, rect.min.y + minimap_h + 7.0),
                        egui::pos2(px + 5.0, rect.min.y + minimap_h + 7.0),
                    ],
                    PLAYHEAD,
                    egui::Stroke::NONE,
                ));

                // --- Empty state hint ---
                if clips.is_empty() {
                    painter.text(
                        egui::pos2(rect.center().x, rect.center().y),
                        egui::Align2::CENTER_CENTER,
                        "Import media or drag clips here (Ctrl+K for commands)",
                        egui::FontId::proportional(12.0),
                        Color32::from_rgb(120, 120, 132),
                    );
                }

                // --- Drag-drop from the media panel ---
                if let Some(src) = self.drag_source {
                    let dragging = ctx.input(|i| i.pointer.primary_down());
                    if dragging {
                        if let Some(p) = ctx.input(|i| i.pointer.hover_pos()) {
                            if rect.contains(p) {
                                let drop_us = t_for(p.x).max(0);
                                self.drag_timeline_us = Some(drop_us);
                                let gx = x_for(drop_us);
                                painter.line_segment(
                                    [egui::pos2(gx, rect.min.y), egui::pos2(gx, rect.max.y)],
                                    egui::Stroke::new(1.0, self.accent()),
                                );
                                if let Some((_kind, _idx, lane)) =
                                    lane_rects.iter().find(|(_, _, l)| l.contains(p))
                                {
                                    painter.rect_stroke(
                                        *lane,
                                        0.0,
                                        egui::Stroke::new(1.0, self.accent()),
                                        StrokeKind::Inside,
                                    );
                                }
                            }
                        }
                    }
                    if ctx.input(|i| i.pointer.any_released()) {
                        let mut drop_us =
                            self.drag_timeline_us.unwrap_or(self.project.duration_us());
                        if self.settings.snap_to_playhead
                            && (drop_us - self.playhead_us).abs() < 150_000
                        {
                            drop_us = self.playhead_us;
                        }
                        if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                            if rect.contains(p) {
                                let mut target: Option<(u8, usize)> = None;
                                for (kind, idx, lane) in &lane_rects {
                                    if lane.contains(p) {
                                        target = Some((*kind, *idx));
                                        break;
                                    }
                                }
                                if let Some((kind, idx)) = target {
                                    self.push_undo();
                                    if kind == 2 {
                                        self.add_text_asset_drop(&src, idx, drop_us);
                                    } else {
                                        self.add_clip_at(src, kind, idx, drop_us);
                                    }
                                }
                            }
                        }
                        self.drag_source = None;
                        self.drag_timeline_us = None;
                    }
                }
            });
    }

    /// New empty text clip dropped onto a text lane.
    fn add_text_asset_drop(&mut self, _src: &AssetId, _track: usize, _time: i64) {
        self.add_text_clip_at(_track, _time);
    }

    fn add_text_clip_at(&mut self, track: usize, time: i64) {
        let idx = track.min(self.project.text_tracks.len().saturating_sub(1));
        self.project.text_tracks[idx].clips.push(TextClip {
            text: "New text".into(),
            timeline_start: time.max(0),
            timeline_end: time.max(0) + 3 * SECOND_US,
            ..TextClip::default()
        });
    }

    /// Snap a proposed clip start time to the playhead, marker, or a nearby
    /// clip edge (within ~8px worth of time).
    fn snapped_time(&self, t: i64) -> i64 {
        if !self.settings.snap_to_playhead {
            return t;
        }
        let window = (self.timeline_zoom.max(4.0).recip() * SECOND_US as f32 * 9.0) as i64;
        let mut best = t;
        let mut best_d = window + 1;
        let mut candidates: Vec<i64> = Vec::new();
        candidates.push(self.playhead_us);
        for m in &self.project.markers {
            candidates.push(m.time_us);
        }
        for tr in &self.project.video_tracks {
            for c in &tr.clips {
                candidates.push(c.timeline_start);
                candidates.push(c.timeline_start + c.on_timeline_us());
            }
        }
        for tr in &self.project.audio_tracks {
            for c in &tr.clips {
                candidates.push(c.timeline_start);
                candidates.push(c.timeline_start + c.on_timeline_us());
            }
        }
        for tr in &self.project.text_tracks {
            for c in &tr.clips {
                candidates.push(c.timeline_start);
                candidates.push(c.timeline_end);
            }
        }
        for cand in candidates {
            let d = (cand - t).abs();
            if d <= window && d < best_d {
                best = cand;
                best_d = d;
            }
        }
        best
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
        let ext = std::path::Path::new(&path_s)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "srt" || ext == "vtt" {
            if self.import_subtitles(&path_s) {
                self.toasts.push(("Imported subtitles as text clips".into(), Instant::now()));
            } else {
                self.toasts.push(("Subtitle import failed".into(), Instant::now()));
            }
            return;
        }
        match crate::assets::import_media(&mut self.project.assets, &path_s) {
            Ok(id) => {
                // If the source is a video with a different frame rate, ask
                // whether to adopt it as the project fps (dialog, not silent).
                let mut tag = String::new();
                if let Some(a) = self.project.assets.get(id) {
                    if a.kind == AssetKind::Video && a.frame_rate > 0.0 {
                        if (a.frame_rate - self.project.fps).abs() > 0.001 {
                            self.pending_fps = Some(a.frame_rate);
                        }
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

    /// Import an SRT/VTT subtitle file as text clips on a new text track.
    fn import_subtitles(&mut self, path: &str) -> bool {
        let Ok(data) = std::fs::read_to_string(path) else {
            return false;
        };
        let subs = crate::text::parse_subtitles(&data);
        if subs.is_empty() {
            return false;
        }
        self.push_undo();
        let track_idx = self.project.text_tracks.len();
        self.project.text_tracks.push(crate::timeline::Track {
            name: "Subtitles".to_string(),
            clips: Vec::new(),
            ..crate::timeline::Track::default()
        });
        for s in &subs {
            let mut style = TextStyle::default();
            style.font_size = 21.0;
            style.color = [255, 255, 255, 255];
            style.outline_color = [0, 0, 0, 200];
            style.outline_width = 2.0;
            style.shadow = true;
            style.word_wrap = 0.0;
            let mut tr = Transform::default();
            tr.y = 0.85;
            self.project.text_tracks[track_idx].clips.push(TextClip {
                text: s.text.clone(),
                timeline_start: s.start_us,
                timeline_end: s.end_us,
                style,
                transform: tr,
                opacity: 1.0,
            });
        }
        self.selected = Selection::None;
        true
    }

    fn new_project(&mut self) {
        self.push_undo();
        self.project = Project::new();
        self.selected = Selection::None;
        self.multi_selection.clear();
        self.clipboard = None;
        self.last_request = None;
        self.playhead_us = 0;
        self.audio.refresh(&self.project);
        crate::prefs::clear_autosave();
        self.current_project_path = None;
        self.dirty = false;
        self.toasts.push(("New project".into(), Instant::now()));
    }

    fn prompt_open_project(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("rsedit project", &["rsedit"])
            .pick_file()
        {
            self.load_project_file(&p.to_string_lossy().into_owned());
        }
    }

    fn load_project_file(&mut self, path: &str) {
        match crate::prefs::load_project_file(path) {
            Ok(p) => {
                self.push_undo();
                self.project = p;
                self.selected = Selection::None;
                self.multi_selection.clear();
                self.last_request = None;
                self.playhead_us = 0;
                self.audio.refresh(&self.project);
                crate::prefs::clear_autosave();
                self.current_project_path = Some(path.to_string());
                self.dirty = false;
                self.add_recent(path);
                self.toasts.push((format!("Opened {path}"), Instant::now()));
            }
            Err(e) => self.toasts.push((format!("Open failed: {e:#}"), Instant::now())),
        }
    }

    pub fn save_project(&mut self) {
        if let Some(p) = self.current_project_path.clone() {
            match crate::prefs::save_project_file(&p, &self.project) {
                Ok(()) => {
                    crate::prefs::clear_autosave();
                    self.dirty = false;
                    self.autosave_timer = Instant::now();
                    self.add_recent(&p);
                    self.toasts.push(("Project saved".into(), Instant::now()));
                }
                Err(e) => self.toasts.push((format!("Save failed: {e:#}"), Instant::now())),
            }
        } else {
            self.save_project_as();
        }
    }

    pub fn save_project_as(&mut self) {
        let mut default = self.current_project_path.clone().unwrap_or_else(|| "project.rsedit".into());
        if !default.ends_with(".rsedit") {
            default.push_str(".rsedit");
        }
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("rsedit project", &["rsedit"])
            .set_file_name(default)
            .save_file()
        {
            let p = p.to_string_lossy().into_owned();
            match crate::prefs::save_project_file(&p, &self.project) {
                Ok(()) => {
                    crate::prefs::clear_autosave();
                    self.current_project_path = Some(p.clone());
                    self.dirty = false;
                    self.autosave_timer = Instant::now();
                    self.add_recent(&p);
                    self.toasts.push((format!("Saved as {p}"), Instant::now()));
                }
                Err(e) => self.toasts.push((format!("Save failed: {e:#}"), Instant::now())),
            }
        }
    }

    fn add_recent(&mut self, path: &str) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_string());
        self.recent_files.truncate(8);
        let _ = crate::prefs::write_config(
            self.settings,
            &self.keybinds.binds(),
            &self.recent_files,
        );
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
                            ..VideoClip::default()
                        });
                    }
                    _ => {
                        self.project.video_tracks[idx].clips.push(VideoClip {
                            asset: AssetId(id.0),
                            source_in: 0,
                            source_out: dur,
                            timeline_start: time_us,
                            ..VideoClip::default()
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
                    fade_in_us: 200_000,
                    fade_out_us: 200_000,
                    ..AudioClip::default()
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
                ..c.clone()
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
                ..c.clone()
            },
        );
        self.audio.refresh(&self.project);
    }

    // ── Export dialog ────────────────────────────────────────────────────────
    fn export_window(&mut self, ctx: &egui::Context) {
        let mut open = self.export_open;
        let msg = self.export_msg.clone();
        egui::Window::new("Export")
            .open(&mut open)
            .collapsible(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                if self.export_running {
                    ui.label("Rendering…");
                    ui.add(
                        egui::ProgressBar::new(self.export_progress)
                            .desired_width(ui.available_width()),
                    );
                    if ui.button("Cancel").clicked() {
                        self.export_cancel
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                } else {
                    ui.label("Render the timeline to a video or audio file.");
                    ui.horizontal(|ui| {
                        ui.label("File:");
                        ui.text_edit_singleline(&mut self.export_path);
                        if ui.button("Browse").clicked() {
                            let filter = if self.export_audio_only { "m4a" } else { "mp4" };
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("out", &[filter])
                                .set_file_name(if self.export_audio_only { "out.m4a" } else { "out.mp4" })
                                .save_file()
                            {
                                self.export_path = p.to_string_lossy().into_owned();
                            }
                        }
                    });
                    const PRESETS: [(&str, u32, u32, f64); 5] = [
                        ("1080p 30", 1920, 1080, 30.0),
                        ("1080p 60", 1920, 1080, 60.0),
                        ("4K 30", 3840, 2160, 30.0),
                        ("9:16 1080p", 1080, 1920, 30.0),
                        ("Custom", 0, 0, 0.0),
                    ];
                    let mut preset_idx = PRESETS.len() - 1;
                    for (i, (name, w, h, f)) in PRESETS.iter().enumerate() {
                        if *name != "Custom" && self.export_w == *w && self.export_h == *h
                            && (self.export_fps - f).abs() < 0.01
                        {
                            preset_idx = i;
                            break;
                        }
                    }
                    egui::ComboBox::from_label("Preset")
                        .selected_text(PRESETS[preset_idx].0)
                        .show_ui(ui, |ui| {
                            for (i, (name, w, h, f)) in PRESETS.iter().enumerate() {
                                if ui.selectable_label(preset_idx == i, *name).clicked() {
                                    if *name != "Custom" {
                                        self.export_w = *w;
                                        self.export_h = *h;
                                        self.export_fps = *f;
                                    }
                                }
                            }
                        });

                    ui.checkbox(&mut self.export_audio_only, "Audio only (no video)");
                    if !self.export_audio_only {
                        egui::Grid::new("export_grid").num_columns(2).show(ui, |inner| {
                            inner.label("Width");
                            inner.add(egui::DragValue::new(&mut self.export_w).range(320..=3840));
                            inner.end_row();
                            inner.label("Height");
                            inner.add(egui::DragValue::new(&mut self.export_h).range(240..=2160));
                            inner.end_row();
                        });
                        egui::ComboBox::from_label("Codec")
                            .selected_text(if self.export_codec == 1 { "VP9" } else { "H.264" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_codec, 0, "H.264");
                                ui.selectable_value(&mut self.export_codec, 1, "VP9");
                            });
                    }
                    egui::Grid::new("export_grid2").num_columns(2).show(ui, |inner| {
                        inner.label("FPS");
                        inner.add(egui::DragValue::new(&mut self.export_fps).range(10.0..=60.0));
                        inner.end_row();
                        if !self.export_audio_only {
                            inner.label("Bitrate (kbps)");
                            inner.add(egui::DragValue::new(&mut self.export_bitrate).range(500..=40000));
                            inner.end_row();
                        }
                    });
                    let has_loop = self.project.loop_start.is_some() && self.project.loop_end.is_some();
                    ui.add_enabled(
                        has_loop,
                        egui::Checkbox::new(&mut self.export_range, "Export loop region only"),
                    );
                    let clickable = self.export_path.is_empty()
                        || (self.export_audio_only
                            && !self.export_path.to_lowercase().ends_with(".m4a"));
                    if ui
                        .add_enabled(
                            !clickable,
                            egui::Button::new("Render").fill(self.accent()),
                        )
                        .clicked()
                    {
                        let project = self.project.clone();
                        let (from_us, to_us) = if self.export_range {
                            (
                                self.project.loop_start.unwrap_or(0),
                                self.project.loop_end.unwrap_or(0),
                            )
                        } else {
                            (0, 0)
                        };
                        let mut path = self.export_path.clone();
                        if self.export_audio_only && !path.to_lowercase().ends_with(".m4a") {
                            path = path.replace(".mp4", ".m4a");
                        }
                        let cfg = crate::export::ExportConfig {
                            path,
                            width: self.export_w,
                            height: self.export_h,
                            fps: self.export_fps,
                            bitrate_kbps: self.export_bitrate,
                            audio_only: self.export_audio_only,
                            vp9: self.export_codec == 1,
                            from_us,
                            to_us,
                        };
                        self.export_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                        *self.export_progress_cell.lock().unwrap() = 0.0;
                        self.export_progress = 0.0;
                        self.export_msg = None;
                        self.export_running = true;
                        let cancel = self.export_cancel.clone();
                        let cell = self.export_progress_cell.clone();
                        self.export_worker = Some(std::thread::spawn(move || {
                            crate::export::export_project(&project, &cfg, &cancel, &mut |p| {
                                *cell.lock().unwrap() = p;
                            })
                        }));
                    }
                }
                if let Some(m) = &msg {
                    ui.separator();
                    ui.label(m);
                }
            });
        self.export_open = open;
    }

    // ── Preferences dialog (editable keybinds + global settings) ─────────────
    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        let prev_dark = self.settings.dark_mode;
        let prev_accent = self.settings.accent;
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
                ui.checkbox(&mut self.settings.autosave, "Autosave project");
                ui.checkbox(&mut self.settings.safe_margins, "Show safe margins in preview");
                ui.horizontal(|ui| {
                    ui.label("Accent color");
                    let mut c = egui::Color32::from_rgb(
                        self.settings.accent[0],
                        self.settings.accent[1],
                        self.settings.accent[2],
                    );
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        self.settings.accent = [c.r(), c.g(), c.b()];
                    }
                    ui.label("Theme accent applies to selections and highlights.");
                });
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
        if prev_dark != self.settings.dark_mode || prev_accent != self.settings.accent {
            apply_theme(ctx, self.settings.dark_mode, self.settings.accent);
            let _ = crate::prefs::write_config(
                self.settings,
                &self.keybinds.binds(),
                &self.recent_files,
            );
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

    // ── Frame rate adjustment dialog ─────────────────────────────────────────
    fn fps_prompt_window(&mut self, ctx: &egui::Context) {
        let Some(fps) = self.pending_fps else { return };
        let mut open = true;
        let mut decided = false;
        egui::Window::new("Adjust frame rate?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "The imported video runs at {:.2} fps.\nAdjust the project to \
                     match ({:.2} fps)?",
                    fps, fps
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let yes = egui::Button::new(format!("Yes · {:.2} fps", fps))
                        .fill(ui.visuals().selection.bg_fill);
                    if ui.add(yes).clicked() {
                        self.project.fps = fps;
                        self.toasts.push((
                            format!("Project frame rate set to {:.2} fps", fps),
                            Instant::now(),
                        ));
                        decided = true;
                    }
                    if ui.button("No").clicked() {
                        self.toasts.push((
                            "Kept current project frame rate".into(),
                            Instant::now(),
                        ));
                        decided = true;
                    }
                });
            });
        // Closing via X behaves like "No".
        if decided || !open {
            self.pending_fps = None;
        }
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
const GRAY: Color32 = Color32::from_rgb(170, 174, 182);
const DIM: Color32 = Color32::from_rgb(110, 114, 122);
const BUTTON: Color32 = Color32::from_rgb(32, 32, 38);
const ACCENT: Color32 = Color32::from_rgb(60, 120, 210);
const VIDEO_CLIP: Color32 = Color32::from_rgb(40, 70, 120);
const AUDIO_CLIP: Color32 = Color32::from_rgb(46, 108, 74);
const TEXT_CLIP: Color32 = Color32::from_rgb(120, 90, 150);
const WAVE: Color32 = Color32::from_rgb(120, 220, 170);
const PLAYHEAD: Color32 = Color32::from_rgb(255, 80, 80);

fn apply_theme(ctx: &egui::Context, dark: bool, accent: [u8; 3]) {
    let mut style = (*ctx.style()).clone();
    style.visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let sel_bg = if dark {
        Color32::from_rgb(
            (accent[0] as u32 * 45 / 100 + 10) as u8,
            (accent[1] as u32 * 45 / 100 + 10) as u8,
            (accent[2] as u32 * 45 / 100 + 10) as u8,
        )
    } else {
        Color32::from_rgb(
            (accent[0] as u16 / 2 + 127) as u8,
            (accent[1] as u16 / 2 + 127) as u8,
            (accent[2] as u16 / 2 + 127) as u8,
        )
    };
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
    }
    style.visuals.selection.bg_fill = sel_bg;
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    ctx.set_style(style);
}