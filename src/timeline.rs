//! Timeline data model — the serde-serializable core of the editor.
//!
//! Mirrors the sketch in rsedit.md: a set of independent time-aligned tracks
//! (video/image, audio, text), each holding an ordered list of clip instances
//! referencing source assets.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Time position in microseconds. Integer math keeps everything sample-accurate.
pub type Timecode = i64;

pub const SECOND_US: Timecode = 1_000_000;

/// Default project settings (spec open-question default: 48kHz f32 internal).
pub const DEFAULT_FPS: f64 = 30.0;
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Video,
    Audio,
    Image,
}

/// A source asset on disk (video file, audio file, or static image).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub kind: AssetKind,
    pub name: String,
    pub path: String,
    pub duration_us: Timecode,
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub sample_rate: u32,
    pub channels: u32,
    /// Downsampled waveform peaks (min,max,N) per bin, generated on import.
    pub peaks: Vec<(f32, f32)>,
    /// For Image assets: decoded RGBA8 pixels, kept in memory.
    #[serde(skip)]
    pub rgba: Option<Vec<u8>>,
    /// Small RGBA8 thumbnail (w, h, pixels). Video = first decoded frame;
    /// image = downscaled source; audio = none.
    #[serde(skip)]
    pub thumb: Option<(u32, u32, Vec<u8>)>,
    /// For Audio assets: fully decoded interleaved f32 PCM at project rate.
    #[serde(skip)]
    pub pcm: Option<Vec<f32>>,
}

impl Asset {
    #[allow(dead_code)]
    pub fn image(&self) -> Option<&[u8]> {
        self.rgba.as_deref()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetStore {
    pub assets: HashMap<AssetId, Asset>,
    next_id: u64,
}

impl AssetStore {
    pub fn new() -> Self {
        Self {
            assets: HashMap::new(),
            next_id: 1,
        }
    }
    pub fn insert(&mut self, mut a: Asset) -> AssetId {
        if a.id.0 == 0 {
            a.id = AssetId(self.next_id);
            self.next_id += 1;
        } else {
            self.next_id = self.next_id.max(a.id.0 + 1);
        }
        let id = a.id;
        self.assets.insert(id, a);
        id
    }
    pub fn get(&self, id: AssetId) -> Option<&Asset> {
        self.assets.get(&id)
    }
    pub fn len(&self) -> usize {
        self.assets.len()
    }
}

/// Position + scale + rotation for image/video/text placements.
/// `x,y` are normalized [0,1] centers within the composition.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    /// Rotation in degrees (positive = clockwise).
    pub rotate: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: 0.5,
            scale: 1.0,
            rotate: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoClip {
    pub asset: AssetId,
    pub source_in: Timecode,
    pub source_out: Timecode,
    pub timeline_start: Timecode,
    pub opacity: f32,
    pub transform: Transform,
}

impl Default for VideoClip {
    fn default() -> Self {
        Self {
            asset: AssetId(0),
            source_in: 0,
            source_out: 0,
            timeline_start: 0,
            opacity: 1.0,
            transform: Transform::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioClip {
    pub asset: AssetId,
    pub source_in: Timecode,
    pub source_out: Timecode,
    pub timeline_start: Timecode,
    pub gain_db: f32,
    pub fade_in_us: Timecode,
    pub fade_out_us: Timecode,
}

impl Default for AudioClip {
    fn default() -> Self {
        Self {
            asset: AssetId(0),
            source_in: 0,
            source_out: 0,
            timeline_start: 0,
            gain_db: 0.0,
            fade_in_us: 0,
            fade_out_us: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

fn default_font_family() -> String {
    "DejaVu Sans".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextStyle {
    pub font_size: f32,
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_text_color")]
    pub color: [u8; 4],
    pub align: TextAlign,
}

fn default_text_color() -> [u8; 4] {
    [236, 236, 240, 255]
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_size: 24.0,
            font_family: default_font_family(),
            color: [236, 236, 240, 255],
            align: TextAlign::Center,
        }
    }
}

fn default_opacity() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextClip {
    pub text: String,
    pub timeline_start: Timecode,
    pub timeline_end: Timecode,
    pub style: TextStyle,
    #[serde(default = "default_transform")]
    pub transform: Transform,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
}

fn default_transform() -> Transform {
    Transform::default()
}

impl Default for TextClip {
    fn default() -> Self {
        Self {
            text: String::new(),
            timeline_start: 0,
            timeline_end: 0,
            style: TextStyle::default(),
            transform: Transform::default(),
            opacity: 1.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Track<T> {
    pub clips: Vec<T>,
    pub muted: bool,
    pub locked: bool,
    pub name: String,
}

/// A whole editing project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub video_tracks: Vec<Track<VideoClip>>,
    pub audio_tracks: Vec<Track<AudioClip>>,
    pub text_tracks: Vec<Track<TextClip>>,
    pub assets: AssetStore,
    pub fps: f64,
    pub sample_rate: u32,
    pub width: u32,
    pub height: u32,
}

impl Project {
    pub fn new() -> Self {
        let mut p = Self::default();
        p.fps = DEFAULT_FPS;
        p.sample_rate = DEFAULT_SAMPLE_RATE;
        p.width = 1920;
        p.height = 1080;
        p.video_tracks.push(Track {
            name: "V1".into(),
            ..Default::default()
        });
        p.audio_tracks.push(Track {
            name: "A1".into(),
            ..Default::default()
        });
        p.text_tracks.push(Track {
            name: "T1".into(),
            ..Default::default()
        });
        p
    }

    pub fn duration_us(&self) -> Timecode {
        let mut end = 0;
        for t in &self.video_tracks {
            for c in &t.clips {
                end = end.max(c.timeline_start + (c.source_out - c.source_in));
            }
        }
        for t in &self.audio_tracks {
            for c in &t.clips {
                end = end.max(c.timeline_start + (c.source_out - c.source_in));
            }
        }
        for t in &self.text_tracks {
            for c in &t.clips {
                end = end.max(c.timeline_end);
            }
        }
        end
    }

    /// Video clips active at time `t`, sorted by track index (bottom first).
    #[allow(dead_code)]
    pub fn active_video_clips(&self, t: Timecode) -> Vec<(usize, &VideoClip)> {
        let mut out = Vec::new();
        for (ti, track) in self.video_tracks.iter().enumerate() {
            if track.locked {
                continue;
            }
            for c in &track.clips {
                let end = c.timeline_start + (c.source_out - c.source_in);
                if c.timeline_start <= t && t < end {
                    out.push((ti, c));
                }
            }
        }
        out
    }

    #[allow(dead_code)]
    pub fn active_audio_clips(&self, t: Timecode) -> Vec<(usize, &AudioClip)> {
        let mut out = Vec::new();
        for (ti, track) in self.audio_tracks.iter().enumerate() {
            if track.muted {
                continue;
            }
            for c in &track.clips {
                let end = c.timeline_start + (c.source_out - c.source_in);
                if c.timeline_start <= t && t < end {
                    out.push((ti, c));
                }
            }
        }
        out
    }

    #[allow(dead_code)]
    pub fn active_text_clips(&self, t: Timecode) -> Vec<&TextClip> {
        self.text_tracks
            .iter()
            .filter(|t| !t.locked)
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.timeline_start <= t && t < c.timeline_end)
            .collect()
    }

    /// Map timeline time to source time within a clip. `None` when clip active but
    /// asset missing or time beyond source.
    #[allow(dead_code)]
    pub fn clip_source_offset(&self, clip: &VideoClip, t: Timecode) -> Option<Timecode> {
        let rel = t - clip.timeline_start;
        let src = clip.source_in + rel;
        if src < clip.source_out {
            Some(src)
        } else {
            None
        }
    }
}

impl Default for AssetStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_and_clip_roundtrip() {
        let mut p = Project::new();
        let id = p.assets.insert(Asset {
            id: AssetId(0),
            kind: AssetKind::Video,
            name: "a.mp4".into(),
            path: "/tmp/a.mp4".into(),
            duration_us: 10 * SECOND_US,
            width: 1920,
            height: 1080,
            frame_rate: 30.0,
            sample_rate: 48000,
            channels: 2,
            peaks: vec![],
            rgba: None,
            thumb: None,
            pcm: None,
        });
        p.video_tracks[0].clips.push(VideoClip {
            asset: id,
            source_in: 0,
            source_out: 5 * SECOND_US,
            timeline_start: 1 * SECOND_US,
            opacity: 1.0,
            transform: Transform::default(),
        });
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.video_tracks[0].clips.len(), 1);
        assert_eq!(back.duration_us(), 6 * SECOND_US);
        assert_eq!(back.active_video_clips(2 * SECOND_US).len(), 1);
        assert!(back.active_video_clips(8 * SECOND_US).is_empty());
    }
}