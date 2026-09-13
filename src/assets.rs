//! Asset import: media files become timeline-ready Assets (probe, decode, peaks).

use anyhow::{Context as _, Result};
use std::path::Path;

use crate::decoder;
use crate::timeline::{Asset, AssetId, AssetKind, AssetStore};

/// Import a video or audio file. If the file has audio we decode it to PCM and
/// build waveform peaks (per spec). Images go through load_image_rgba.
pub fn import_media(store: &mut AssetStore, path: &str) -> Result<AssetId> {
    let name = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());

    decoder::ensure_ffmpeg();

    let is_image = {
        let ext = Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        matches!(ext.as_str(), "png" | "webp" | "tif" | "tiff" | "jpg" | "jpeg" | "bmp" | "gif")
    };

    if is_image {
        let (w, h, rgba) =
            decoder::load_image_rgba(path).with_context(|| format!("import image {path}"))?;
        let (tw, th) = fit_box(w, h, 96, 54);
        let thumb = Some((
            tw,
            th,
            decoder::scale_rgba(&rgba, w, h, tw, th),
        ));
        let duration_us = crate::timeline::SECOND_US; // single held frame for clip duration
        return Ok(store.insert(Asset {
            id: AssetId(0),
            kind: AssetKind::Image,
            name,
            path: path.to_string(),
            duration_us,
            width: w,
            height: h,
            frame_rate: 0.0,
            sample_rate: 0,
            channels: 1,
            peaks: vec![],
            rgba: Some(rgba),
            thumb,
            pcm: None,
        }));
    }

    let has_video = decoder::has_video_stream(path).unwrap_or(false);
    let has_audio = decoder::has_audio_stream(path).unwrap_or(false);

    if !has_video && !has_audio {
        anyhow::bail!("no supported video/audio stream in {path}");
    }

    let (duration_us, fps, w, h) = decoder::probe(path).unwrap_or((0, 0.0, 0, 0));

    let kind = if has_video {
        AssetKind::Video
    } else {
        AssetKind::Audio
    };

    // Decode audio fully for waveform + mixing. Cap memory: decode at most
    // ~5 minutes of audio to keep import snappy.
    let mut pcm = None;
    let mut peaks = vec![];
    let mut audio_channels = 0u32;
    let mut audio_rate = 0u32;
    if has_audio {
        let cap_seconds = if kind == AssetKind::Video { 300 } else { i32::MAX as usize };
        if let Ok((channels, samples)) = decoder::decode_audio_full(path, 48_000) {
            audio_channels = channels;
            audio_rate = 48_000;
            let cap = channels as usize * 48_000 * cap_seconds.min(300);
            pcm = Some(if samples.len() > cap {
                samples[..cap].to_vec()
            } else {
                samples
            });
            peaks = decoder::compute_peaks(pcm.as_deref().unwrap_or(&[]), channels, 4096);
        }
    }

    // First-frame thumbnail for video assets (aspect-preserving, max 96x54).
    let thumb = if kind == AssetKind::Video && w > 0 && h > 0 {
        let (tw, th) = fit_box(w, h, 96, 54);
        decoder::video_thumbnail(path, tw, th).ok()
    } else {
        None
    };

    Ok(store.insert(Asset {
        id: AssetId(0),
        kind,
        name,
        path: path.to_string(),
        duration_us,
        width: w,
        height: h,
        frame_rate: fps,
        sample_rate: audio_rate,
        channels: audio_channels.max(1),
        peaks,
        rgba: None,
        thumb,
        pcm,
    }))
}

/// Largest (tw, th) that fits inside the box while keeping `(w, h)` aspect.
fn fit_box(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (max_w, max_h);
    }
    let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32).min(1.0);
    (
        ((w as f32 * scale).round()).max(1.0) as u32,
        ((h as f32 * scale).round()).max(1.0) as u32,
    )
}