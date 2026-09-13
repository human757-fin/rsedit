//! Preferences / recent-files / autosave persistence to JSON files under the
//! user config directory (`~/.config/rsedit`). Loaded at startup and
//! written whenever settings or recent-files change.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::{BindField, Settings};
use crate::timeline::Project;

#[derive(Serialize, Deserialize)]
pub struct PreferencesFile {
    pub settings: Settings,
    /// (bind label, ctrl, shift, alt, key name)
    pub keybinds: Vec<(String, bool, bool, bool, String)>,
    pub recent: Vec<String>,
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("rsedit")
}

fn prefs_path() -> PathBuf {
    config_dir().join("prefs.json")
}

pub fn autosave_path() -> PathBuf {
    config_dir().join("autosave.rsedit")
}

pub fn load() -> Option<PreferencesFile> {
    let data = std::fs::read_to_string(prefs_path()).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn save(prefs: &PreferencesFile) {
    if let Ok(data) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(prefs_path(), data);
    }
}

pub fn write_config(
    settings: Settings,
    keybinds: &[(String, bool, bool, bool, String)],
    recent: &[String],
) {
    save(&PreferencesFile {
        settings,
        keybinds: keybinds.to_vec(),
        recent: recent.to_vec(),
    });
}

/// Persist a project to the autosave location.
pub fn save_autosave(project: &Project) {
    if let Ok(data) = serde_json::to_string(project) {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(autosave_path(), data);
    }
}

/// Save a project to a user-chosen `.rsedit` file.
pub fn save_project_file(path: &str, project: &Project) -> anyhow::Result<()> {
    let data = serde_json::to_string_pretty(project)?;
    std::fs::write(path, data)?;
    Ok(())
}

/// Load a `.rsedit` project file.
pub fn load_project_file(path: &str) -> anyhow::Result<Project> {
    let data = std::fs::read_to_string(path)?;
    let p: Project = serde_json::from_str(&data)?;
    Ok(p)
}

pub fn load_autosave() -> Option<Project> {
    let data = std::fs::read_to_string(autosave_path()).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn clear_autosave() {
    let _ = std::fs::remove_file(autosave_path());
}

/// Map a persisted bind label back to the field it controls.
pub fn keybind_field(name: &str) -> Option<BindField> {
    use crate::app::BindField::*;
    Some(match name {
        "Play / Pause" => PlayPause,
        "Step one frame back" => StepBack,
        "Step one frame forward" => StepFwd,
        "Jump to start" => JumpStart,
        "Jump to end" => JumpEnd,
        "Split clip at playhead" => SplitClip,
        "Delete selected clip" => DeleteClip,
        "Undo" => Undo,
        "Redo" => Redo,
        "Open / close export" => ExportRender,
        "Add text clip" => AddText,
        "Toggle audio peaks" => TogglePeaks,
        "Add video track" => AddVideoTrack,
        "Add audio track" => AddAudioTrack,
        "Close dialogs" => CloseWindow,
        _ => return None,
    })
}

/// Decode a persisted key name into an egui key.
pub fn key_from_str(s: &str) -> Option<eframe::egui::Key> {
    use eframe::egui::Key::*;
    Some(match s {
        "Space" => Space,
        "ArrowLeft" => ArrowLeft,
        "ArrowRight" => ArrowRight,
        "ArrowUp" => ArrowUp,
        "ArrowDown" => ArrowDown,
        "Home" => Home,
        "End" => End,
        "S" => S,
        "Z" => Z,
        "Delete" => Delete,
        "X" => X,
        "C" => C,
        "V" => V,
        "E" => E,
        "T" => T,
        "P" => P,
        "A" => A,
        "K" => K,
        "F" => F,
        "Escape" => Escape,
        "Enter" => Enter,
        _ => return None,
    })
}