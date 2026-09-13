//! Export: render the timeline to an MP4 (H.264 + AAC) via FFmpeg.
//!
//! Video frames come from the same CPU compositor the preview uses
//! (`render.rs`), guaranteeing WYSIWYG. Audio is re-rendered from the same
//! in-memory mix used by playback.

use std::collections::HashMap;

use anyhow::{Context as _, Result, bail};
use ffmpeg_next as ffmpeg;
use ffmpeg::codec::Id as CodecId;
use ffmpeg::format::Pixel;
use ffmpeg::software::resampling;
use ffmpeg::software::scaling::{Context as ScalingContext, Flags as ScaleFlags};

use crate::audio::build_snapshot;
use crate::decoder::VideoSource;
use crate::render;
use crate::timeline::{AssetKind, Project};

pub struct ExportConfig {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub bitrate_kbps: u32,
    pub audio_only: bool,
    /// Use the VP9 encoder instead of H.264.
    pub vp9: bool,
    /// Export the `[from_us, to_us)` segment only. `to_us <= from_us` = full range.
    pub from_us: i64,
    pub to_us: i64,
}

/// Render the (configured segment of the) timeline to the configured file.
/// `progress` receives 0..1; `cancel` aborts with an error when raised.
pub fn export_project(
    project: &Project,
    cfg: &ExportConfig,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    if project.duration_us() <= 0 {
        bail!("timeline is empty");
    }
    let from = cfg.from_us.max(0).min(project.duration_us());
    let to = if cfg.to_us > cfg.from_us {
        cfg.to_us.min(project.duration_us())
    } else {
        project.duration_us()
    };
    let seg_len = (to - from).max(1);

    let rate_us: i64 = (1_000_000.0 / cfg.fps) as i64;
    let total_frames = ((seg_len as f64 / rate_us as f64).ceil() as u64).max(1);

    let vcodec = if cfg.vp9 {
        ffmpeg::codec::encoder::find(CodecId::VP9).context("VP9 encoder not available")
    } else {
        ffmpeg::codec::encoder::find(CodecId::H264).context("H264 encoder not available")
    };

    let mut octx = ffmpeg::format::output(&cfg.path)
        .with_context(|| format!("cannot create {}", cfg.path))?;

    // ---- Video (skipped for audio-only exports) ----
    let mut venc: Option<ffmpeg::codec::encoder::Video> = None;
    let mut videotb: Option<ffmpeg::Rational> = None;
    let mut vstream_idx = usize::MAX;
    if !cfg.audio_only {
        let vcodec = vcodec?;
        let mut vstream = octx.add_stream(vcodec).context("add video stream")?;
        let mut enc = ffmpeg::codec::context::Context::from_parameters(vstream.parameters())?
            .encoder()
            .video()?;
        enc.set_width(cfg.width);
        enc.set_height(cfg.height);
        enc.set_format(Pixel::YUV420P);
        enc.set_frame_rate(Some(ffmpeg::Rational(cfg.fps as i32, 1)));
        enc.set_time_base(ffmpeg::Rational(1, cfg.fps.max(1.0) as i32));
        let enc = {
            let mut opts = ffmpeg::Dictionary::new();
            opts.set("preset", "medium");
            opts.set("b", &format!("{}k", cfg.bitrate_kbps));
            enc.open_with(opts).context("open video encoder")?
        };
        vstream.set_parameters(&enc);
        vstream_idx = vstream.index();
        videotb = Some(vstream.time_base());
        venc = Some(enc);
    }

    // ---- Audio ----
    let mut audio_out = octx
        .add_stream(ffmpeg::codec::encoder::find(CodecId::AAC))
        .context("AAC encoder not available")?;
    let mut aenc = ffmpeg::codec::context::Context::from_parameters(audio_out.parameters())?
        .encoder()
        .audio()?;
    aenc.set_rate(48_000);
    aenc.set_channel_layout(ffmpeg::channel_layout::ChannelLayout::STEREO);
    let mut aenc = aenc.open().context("open AAC encoder")?;
    audio_out.set_parameters(&aenc);
    let astream_idx = audio_out.index();
    let atb = audio_out.time_base();
    let mut aresampler = resampling::Context::get(
        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
        ffmpeg::channel_layout::ChannelLayout::STEREO,
        48_000,
        aenc.format(),
        aenc.channel_layout(),
        aenc.rate(),
    )
    .context("build audio resampler")?;
    let sample_chunk = aenc.frame_size().max(1) as usize; // per channel

    octx.write_header().context("write header")?;

    let mix = build_snapshot(project);
    let mut dec = ExportDecoder::new(project, cfg.width, cfg.height);
    let total_samples = ((seg_len as f64 * 48_000.0 / 1_000_000.0) as usize).max(1);

    // We need a scaler even in audio-only mode only for video; keep it optional.
    let mut scaler = if cfg.audio_only {
        None
    } else {
        Some(
            ScalingContext::get(
                Pixel::RGBA,
                cfg.width,
                cfg.height,
                Pixel::YUV420P,
                cfg.width,
                cfg.height,
                ScaleFlags::BILINEAR,
            )
            .context("build RGBA->YUV420P scaler")?,
        )
    };

    // ---- Encode loop ----
    for f in 0..total_frames {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("export cancelled");
        }
        let pts_us = from + (f as i64 * rate_us);
        if !cfg.audio_only {
            let rgba = composition_frame(project, &mut dec, pts_us, cfg.width, cfg.height)?;

            let mut src_rgba = ffmpeg::frame::Video::new(Pixel::RGBA, cfg.width, cfg.height);
            src_rgba.data_mut(0).copy_from_slice(&rgba);
            src_rgba.set_format(Pixel::RGBA);

            let mut yuv = ffmpeg::frame::Video::empty();
            if let Some(sc) = scaler.as_mut() {
                sc.run(&src_rgba, &mut yuv).context("scale frame")?;
            }
            yuv.set_kind(ffmpeg::picture::Type::None);
            yuv.set_pts(Some(f as i64));
            if let Some(enc) = venc.as_mut() {
                enc.send_frame(&yuv).context("send video frame")?;
                let vt = videotb.unwrap();
                drain_video(enc, &mut octx, f as i64, vt, vstream_idx)?;
            }
        }

        // Audio for this frame's time span.
        let s0 = ((from as i64 as f64 + (f as i64 * rate_us) as f64) * 48_000.0 / 1_000_000.0)
            as usize;
        let s0_base = (from as i64 as f64 * 48_000.0 / 1_000_000.0) as usize;
        let s1 = (((from as i64 + (f + 1) as i64 * rate_us) as f64).min(to as f64) * 48_000.0
            / 1_000_000.0) as usize;
        let mut s = s0.max(s0_base);
        let span_end = s1.min(total_samples + s0_base);
        while s < span_end {
            let take = (span_end - s).min(sample_chunk);
            let mut samples = vec![0.0f32; sample_chunk * 2];
            let chunk = mix_span(&mix, s, take, 48_000);
            samples[..take * 2].copy_from_slice(&chunk);
            let mut in_f = ffmpeg::frame::Audio::new(
                ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                sample_chunk,
                ffmpeg::channel_layout::ChannelLayout::STEREO,
            );
            in_f.set_rate(48_000);
            let p0 = in_f.plane_mut::<f32>(0);
            p0.copy_from_slice(&samples);
            let mut out_f = ffmpeg::frame::Audio::empty();
            aresampler.run(&in_f, &mut out_f).context("resample audio")?;
            out_f.set_pts(Some(s as i64));
            aenc.send_frame(&out_f).context("send audio frame")?;
            drain_audio(&mut aenc, &mut octx, s as i64, atb, astream_idx)?;
            s += sample_chunk;
        }

        let p = (f + 1) as f32 / total_frames as f32;
        progress(p);
    }

    // Flush encoders.
    if let Some(enc) = venc.as_mut() {
        enc.send_eof()?;
        drain_video(enc, &mut octx, total_frames as i64, videotb.unwrap(), vstream_idx)?;
    }
    aenc.send_eof()?;
    drain_audio(&mut aenc, &mut octx, total_samples as i64, atb, astream_idx)?;

    octx.write_trailer().context("write trailer")?;
    Ok(())
}

fn drain_video(
    enc: &mut ffmpeg::codec::encoder::Video,
    octx: &mut ffmpeg::format::context::Output,
    pts: i64,
    _tb: ffmpeg::Rational,
    stream_idx: usize,
) -> Result<()> {
    let mut encoded = ffmpeg::packet::Packet::empty();
    while enc.receive_packet(&mut encoded).is_ok() {
        encoded.set_stream(stream_idx);
        encoded.set_pts(Some(pts));
        encoded.set_dts(Some(pts));
        encoded.write_interleaved(octx)?;
    }
    Ok(())
}

fn drain_audio(
    enc: &mut ffmpeg::codec::encoder::Audio,
    octx: &mut ffmpeg::format::context::Output,
    pts: i64,
    _tb: ffmpeg::Rational,
    stream_idx: usize,
) -> Result<()> {
    let mut encoded = ffmpeg::packet::Packet::empty();
    while enc.receive_packet(&mut encoded).is_ok() {
        encoded.set_stream(stream_idx);
        encoded.set_pts(Some(pts));
        encoded.set_dts(Some(pts));
        encoded.write_interleaved(octx)?;
    }
    Ok(())
}

/// Render one composition frame (RGBA, cfg-w x cfg-h) for export using the
/// same compositor as the preview. `dec` owns a private decoder pool so assets
/// aren't reopened every frame.
fn composition_frame(
    project: &Project,
    dec: &mut ExportDecoder,
    pts_us: i64,
    w: u32,
    h: u32,
) -> Result<Vec<u8>> {
    let decoder = dec;
    Ok(render::compose_frame(project, w, h, pts_us, |id, path, src, up_to| {
        match decoder.frame(id, path, src, up_to) {
            Ok(Some(f)) => Some(std::sync::Arc::new(f)),
            _ => None,
        }
    }))
}

/// Sum of all active clips at `[sample_start, sample_start+n)` in interleaved
/// stereo f32 at 48kHz.
fn mix_span(mix: &crate::audio::MixSnapshot, start_sample: usize, n: usize, rate: u32) -> Vec<f32> {
    let mut buf = vec![0.0f32; n * 2];
    for clip in &mix.clips {
        let clip_channels = clip.channels.min(2);
        let speed = clip.speed.max(0.01);
        let src_total = (clip.source_out.max(clip.source_in) - clip.source_in) as f64 / speed as f64;
        let start_s = (clip.timeline_start as f64 * rate as f64 / 1_000_000.0f64) as usize;
        let end_s = start_s + (src_total * rate as f64 / 1_000_000.0) as usize;
        for i in 0..n {
            let s = start_sample + i;
            if s < start_s || s >= end_s {
                continue;
            }
            let src_s = s - start_s;
            let local_us = src_s as f64 * 1_000_000.0 / rate as f64;
            let src_off_us = local_us * speed as f64;
            let pcm_idx = (clip.source_in as f64 * 48_000.0 / 1_000_000.0
                + src_off_us * 48_000.0 / 1_000_000.0) as usize;
            let mut g = clip.gain * crate::audio::envelope_gain(&clip.envelope, local_us as i64);
            if clip.fade_in > 0 && local_us < clip.fade_in as f64 {
                g *= (local_us / clip.fade_in as f64) as f32;
            }
            if clip.fade_out > 0 {
                let rem = src_total - local_us;
                if rem < clip.fade_out as f64 && rem >= 0.0 {
                    g *= (rem / clip.fade_out as f64) as f32;
                }
            }
            for ch in 0..clip_channels {
                let pcm_ch = if clip.channels > 1 { ch } else { 0 };
                let idx = pcm_idx.saturating_mul(clip.channels).saturating_add(pcm_ch);
                if idx < clip.pcm.len() {
                    buf[i * 2 + ch] += clip.pcm[idx] * g;
                }
            }
            if clip_channels == 1 {
                let idx = pcm_idx.saturating_mul(clip.channels);
                if idx < clip.pcm.len() {
                    buf[i * 2 + 1] += clip.pcm[idx] * g;
                }
            }
        }
    }
    for v in buf.iter_mut() {
        *v *= mix.master_gain;
    }
    buf
}

struct ExportDecoder {
    sources: HashMap<u64, VideoSource>,
    w: u32,
    h: u32,
}

impl ExportDecoder {
    fn new(project: &Project, w: u32, h: u32) -> Self {
        let mut sources = HashMap::new();
        for (id, asset) in project.assets.assets.iter() {
            if asset.kind == AssetKind::Video {
                if let Some(src) = VideoSource::open(&asset.path, w, h).ok() {
                    sources.insert(id.0, src);
                }
            }
        }
        Self { sources, w, h }
    }

    fn frame(
        &mut self,
        asset: u64,
        path: &str,
        pts_us: i64,
        _up_to: i64,
    ) -> Result<Option<crate::decoder::VideoFrame>> {
        if !self.sources.contains_key(&asset) {
            let src = VideoSource::open(path, self.w, self.h)?;
            self.sources.insert(asset, src);
        }
        let src = self.sources.get_mut(&asset).unwrap();
        src.frame_at(pts_us)
    }
}