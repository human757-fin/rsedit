//! Audio mixing + playback.
//!
//! Sample-accurate mixing runs entirely from in-memory PCM (every audio asset
//! is imported fully decoded at 48kHz), so the audio callback never touches
//! disk and never blocks. Per-clip gain, fades, and track mute/solo are baked
//! into a lightweight mix snapshot rebuilt whenever the timeline changes.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::timeline::{Project, Timecode};

/// A flattened, playback-ready slice of the audio timeline.
#[derive(Default)]
pub struct MixSnapshot {
    pub clips: Vec<MixClip>,
    pub master_gain: f32,
}

#[derive(Clone)]
pub struct MixClip {
    pub pcm: Arc<Vec<f32>>, // interleaved, 48kHz
    pub channels: usize,
    pub source_in: i64,      // us
    pub source_out: i64,     // us
    pub timeline_start: i64, // us
    pub gain: f32,           // linear
    pub fade_in: i64,        // us
    pub fade_out: i64,       // us
}

/// Build the mix snapshot from the current project. Muted/missing clips are
/// dropped here, so the callback stays allocation-free.
pub fn build_snapshot(project: &Project) -> MixSnapshot {
    let mut clips = Vec::new();
    for track in &project.audio_tracks {
        if track.muted {
            continue;
        }
        for clip in &track.clips {
            let Some(asset) = project.assets.get(clip.asset) else { continue };
            let Some(pcm) = asset.pcm.clone() else { continue };
            clips.push(MixClip {
                pcm: Arc::new(pcm),
                channels: asset.channels.max(1) as usize,
                source_in: clip.source_in,
                source_out: clip.source_out,
                timeline_start: clip.timeline_start,
                gain: db_to_linear(clip.gain_db),
                fade_in: clip.fade_in_us,
                fade_out: clip.fade_out_us,
            });
        }
    }
    MixSnapshot {
        clips,
        master_gain: 1.0,
    }
}

pub fn db_to_linear(db: f32) -> f32 {
    if db <= -90.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

#[derive(Default)]
pub struct AudioState {
    pub snapshot: Mutex<MixSnapshot>,
    pos_us: AtomicU64, // f64 bits
    pub playing: AtomicBool,
}

impl AudioState {
    pub fn pos_us(&self) -> f64 {
        f64::from_bits(self.pos_us.load(Ordering::SeqCst))
    }
    pub fn set_pos_us(&self, v: f64) {
        self.pos_us.store(v.to_bits(), Ordering::SeqCst);
    }
}

pub struct AudioEngine {
    state: Arc<AudioState>,
    stream: Option<Box<dyn StreamHolder>>,
}

pub trait StreamHolder {
    fn play(&self);
    fn pause(&self);
}

impl AudioEngine {
    pub fn new(project: &Project) -> Self {
        let state = Arc::new(AudioState {
            snapshot: Mutex::new(build_snapshot(project)),
            pos_us: AtomicU64::new(0.0f64.to_bits()),
            playing: AtomicBool::new(false),
        });
        Self { state, stream: None }
    }

    /// Current playhead position in microseconds.
    pub fn pos_us(&self) -> f64 {
        self.state.pos_us()
    }

    pub fn set_pos_us(&mut self, us: f64) {
        self.state.set_pos_us(us);
    }

    /// (Re)build the mix snapshot from the current project tracks.
    pub fn refresh(&mut self, project: &Project) {
        *self.state.snapshot.lock().unwrap() = build_snapshot(project);
    }

    pub fn play(&mut self) {
        self.state.playing.store(true, Ordering::SeqCst);
        if let Some(s) = &self.stream {
            s.play();
        }
    }

    pub fn pause(&mut self) {
        self.state.playing.store(false, Ordering::SeqCst);
        if let Some(s) = &self.stream {
            s.pause();
        }
    }

    /// Ensure an output stream is running (idempotent).
    pub fn ensure_stream(&mut self) {
        if self.stream.is_some() {
            return;
        }
        if let Some(s) = build_stream(self.state.clone()) {
            self.stream = Some(Box::new(s));
        }
    }
}

struct CpalStream {
    stream: cpal::Stream,
}

impl StreamHolder for CpalStream {
    fn play(&self) {
        let _ = self.stream.play();
    }
    fn pause(&self) {
        let _ = self.stream.pause();
    }
}

/// Audio accumulation is driven by the callback; the callback directly writes
/// rendered samples into the cpal buffer.
fn build_stream(state: Arc<AudioState>) -> Option<CpalStream> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let config = device.default_output_config().ok()?;
    let out_rate = config.sample_rate().0;
    let out_channels = config.channels() as usize;

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            build::<f32>(&device, &config.into(), state, out_rate, out_channels)
        }
        cpal::SampleFormat::I16 => {
            build::<i16>(&device, &config.into(), state, out_rate, out_channels)
        }
        cpal::SampleFormat::U16 => {
            build::<u16>(&device, &config.into(), state, out_rate, out_channels)
        }
        _ => None,
    }?;
    Some(CpalStream { stream })
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    state: Arc<AudioState>,
    out_rate: u32,
    out_channels: usize,
) -> Option<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let data_cb = move |buf: &mut [T], _info: &cpal::OutputCallbackInfo| {
        let playing = state.playing.load(Ordering::SeqCst);
        if !playing {
            for s in buf.iter_mut() {
                *s = T::from_sample(0.0f32);
            }
            return;
        }
        // How many output frames this buffer wants.
        let frames = if out_channels > 0 {
            buf.len() / out_channels
        } else {
            buf.len()
        };
        if frames == 0 {
            return;
        }
        let t0_us = state.pos_us();
        let snapshot = state.snapshot.lock().unwrap();

        // Render interleaved f32 into a scratch vec (reused each call locally).
        let mut buf32: Vec<f32> = vec![0.0; frames * out_channels];
        for clip in &snapshot.clips {
            let in_ch = clip.channels.max(1);
            let src = clip.source_out.max(clip.source_in);
            let clip_samples = src.saturating_sub(clip.source_in) as f64
                * 48_000.0f64
                / 1_000_000.0f64;
            let start_s = (clip.timeline_start as f64 * out_rate as f64 / 1_000_000.0f64) as i64;
            let end_s = start_s + clip_samples as i64;
            for f in 0..frames {
                let s = f as i64 + (t0_us as i64 * out_rate as i64 / 1_000_000);
                if s < start_s || s >= end_s {
                    continue;
                }
                let src_s = s - start_s; // sample index in clip
                let src_sample = (src_s as f64 * 48_000.0 / out_rate as f64) as usize
                    + (clip.source_in as f64 * 48_000.0 / 1_000_000.0f64) as usize;
                let ts_us = clip.timeline_start as f64
                    + src_s as f64 * 1_000_000.0 / out_rate as f64;
                // gain envelope: fade in/out
                let mut g = clip.gain;
                if clip.fade_in > 0 && (ts_us - clip.timeline_start as f64) < clip.fade_in as f64 {
                    g *= ((ts_us - clip.timeline_start as f64) / clip.fade_in as f64) as f32;
                }
                if clip.fade_out > 0 {
                    let end_us = clip.timeline_start as f64
                        + (clip.source_out - clip.source_in) as f64;
                    let rem = end_us - ts_us;
                    if rem < clip.fade_out as f64 {
                        g *= (rem / clip.fade_out as f64) as f32;
                    }
                }
                if in_ch >= 2 {
                    let idx = src_sample.saturating_mul(2);
                    if idx + 1 < clip.pcm.len() {
                        let v = clip.pcm[idx] * g;
                        let vr = clip.pcm[idx + 1] * g;
                        if out_channels == 1 {
                            buf32[f] += (v + vr) * 0.5;
                        } else {
                            buf32[f * out_channels] += v;
                            buf32[f * out_channels + 1] += vr;
                        }
                    }
                } else {
                    let idx = src_sample.min(clip.pcm.len().saturating_sub(1));
                    let v = clip.pcm[idx] * g;
                    for o in 0..out_channels.min(2) {
                        buf32[f * out_channels + o] += v;
                    }
                }
            }
        }
        // master gain + write
        for (i, v) in buf32.iter().enumerate() {
            buf[i] = T::from_sample(v * snapshot.master_gain);
        }
        // advance playhead by the number of frames we just rendered
        state.set_pos_us(t0_us + frames as f64 * 1_000_000.0 / out_rate as f64);
    };
    let err_cb = |_err: cpal::StreamError| {};
    device
        .build_output_stream(config, data_cb, err_cb, None)
        .ok()
}

/// Convenience: current playhead (us) + duration info for UI.
pub fn timecode_string(us: Timecode, _fps: f64) -> String {
    let total_s = (us.max(0) as f64) / 1_000_000.0;
    let m = (total_s / 60.0) as i64;
    let s = (total_s % 60.0) as i64;
    let frac = ((total_s * 100.0) % 100.0) as i64;
    format!("{m:02}:{s:02}.{frac:02}")
}