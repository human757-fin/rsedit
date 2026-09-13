//! FFmpeg-backed decoding: video -> RGBA8 frames, audio -> interleaved f32.

use anyhow::{Context as _, Result, bail};
use std::path::Path;

use ffmpeg_next as ffmpeg;
use ffmpeg::format::Pixel;
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{Context as ScalingContext, Flags as ScaleFlags};

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub pts_us: i64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Sequential video decoder+scaler pinned to one source. Not Send; owned by one thread.
pub struct VideoSource {
    ictx: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    scaler: ScalingContext,
    stream_index: usize,
    time_base: (i64, i64),
    eof: bool,
    last_pts_out: Option<i64>,
}

impl VideoSource {
    pub fn open(path: &str, target_w: u32, target_h: u32) -> Result<Self> {
        let ictx = ffmpeg::format::input(&path)
            .with_context(|| format!("cannot open {}", path))?;
        let stream = ictx
            .streams()
            .best(Type::Video)
            .context("no video stream")?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let time_base = (tb.0 as i64, tb.1 as i64);
        let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = ctx.decoder().video()?;
        let scaler = ScalingContext::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            Pixel::RGBA,
            target_w,
            target_h,
            ScaleFlags::BILINEAR,
        )?;
        Ok(Self {
            ictx,
            decoder,
            scaler,
            stream_index,
            time_base,
            eof: false,
            last_pts_out: None,
        })
    }

    #[allow(dead_code)]
    pub fn width(&self) -> u32 {
        self.decoder.width()
    }
    #[allow(dead_code)]
    pub fn height(&self) -> u32 {
        self.decoder.height()
    }

    /// Seek to an approximate timestamp (microseconds). Discards any buffered
    /// frames; the next `next_frame()` starts decoding from the seek point.
    pub fn seek_to(&mut self, pts_us: i64) -> Result<()> {
        self.eof = false;
        // avformat_seek_file with stream_index == -1 expects AV_TIME_BASE units.
        let ts = pts_us; // AV_TIME_BASE == 1_000_000 == microseconds
        self.ictx
            .seek(ts, ..)
            .with_context(|| format!("seek to {pts_us}us"))?;
        self.decoder.flush();
        self.last_pts_out = None;
        Ok(())
    }

    fn convert(&mut self, frame: &ffmpeg::frame::Video) -> Result<VideoFrame> {
        let mut rgba = ffmpeg::frame::Video::empty();
        self.scaler.run(frame, &mut rgba)?;
        let raw = frame.pts().unwrap_or(0);
        let pts_us = (raw as f64 * self.time_base.0 as f64 * 1_000_000.0
            / self.time_base.1 as f64) as i64;
        let w = rgba.width();
        let h = rgba.height();
        self.last_pts_out = Some(pts_us);
        Ok(VideoFrame {
            pts_us,
            width: w,
            height: h,
            rgba: rgba.data(0).to_vec(),
        })
    }

    /// Pull the next decoded frame in sequential order, or None at EOF.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        if self.eof {
            return Ok(None);
        }
        loop {
            // Drain whatever the decoder already has buffered.
            let mut frame = ffmpeg::frame::Video::empty();
            if self.decoder.receive_frame(&mut frame).is_ok() {
                return Ok(Some(self.convert(&frame)?));
            }
            // Feed the next packet for our stream.
            let mut fed = false;
            for (s, p) in self.ictx.packets() {
                if s.index() != self.stream_index {
                    continue;
                }
                self.decoder.send_packet(&p).context("send packet")?;
                fed = true;
                break;
            }
            if !fed {
                self.eof = true;
                self.decoder.send_eof()?;
                let mut frame = ffmpeg::frame::Video::empty();
                if self.decoder.receive_frame(&mut frame).is_ok() {
                    return Ok(Some(self.convert(&frame)?));
                }
                return Ok(None);
            }
        }
    }

    /// Decode forward until the frame nearest `pts_us` and return it, seeking
    /// first if `pts_us` is behind the current decode position. Sequential use
    /// keeps this cheap (used by the exporter).
    pub fn frame_at(&mut self, pts_us: i64) -> Result<Option<VideoFrame>> {
        let last = self.last_pts();
        if last.map_or(true, |l| pts_us < l) {
            self.seek_to((pts_us - 100_000).max(0))?;
        }
        let mut best: Option<VideoFrame> = None;
        while let Some(f) = self.next_frame()? {
            match &best {
                None => best = Some(f.clone()),
                Some(b) if (f.pts_us - pts_us).abs() < (b.pts_us - pts_us).abs() => {
                    best = Some(f.clone())
                }
                Some(_) => break,
            }
            if f.pts_us >= pts_us {
                break;
            }
        }
        Ok(best)
    }

    fn last_pts(&self) -> Option<i64> {
        self.last_pts_out
    }
}

/// Decode one audio file fully into interleaved f32 at `out_rate`.
/// Returns (channels, Vec<f32>).
pub fn decode_audio_full(path: &str, out_rate: u32) -> Result<(u32, Vec<f32>)> {
    let mut ictx = ffmpeg::format::input(&path).context("open audio")?;
    let stream = ictx
        .streams()
        .best(Type::Audio)
        .context("no audio stream")?;
    let stream_index = stream.index();
    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
    let mut decoder = ctx.decoder().audio()?;

    let src_rate = decoder.rate();
    let channels = decoder.channels() as u32;
    if src_rate == 0 || channels == 0 {
        bail!("bad audio params");
    }
    let layout = decoder.channel_layout();
    let mut resampler = ffmpeg::software::resampling::Context::get(
        decoder.format(),
        layout,
        src_rate,
        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
        layout,
        out_rate,
    )?;

    let mut out: Vec<f32> = Vec::new();
    let mut frame = ffmpeg::frame::Audio::empty();
    for (s, packet) in ictx.packets() {
        if s.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let mut converted = ffmpeg::frame::Audio::empty();
            resampler.run(&frame, &mut converted)?;
            if converted.is_planar() {
                out.extend_from_slice(&planar_to_interleaved(&converted));
            } else {
                out.extend_from_slice(&pcm_to_f32(converted.data(0)));
            }
        }
    }
    Ok((channels, out))
}

fn compact_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn pcm_to_f32(bytes: &[u8]) -> Vec<f32> {
    compact_f32(bytes)
}

fn planar_to_interleaved(frame: &ffmpeg::frame::Audio) -> Vec<f32> {
    let n = frame.channels() as usize;
    let mut planes: Vec<Vec<f32>> = Vec::new();
    for i in 0..n.max(1) {
        planes.push(compact_f32(frame.data(i)));
    }
    let samples = planes.iter().map(|p| p.len()).max().unwrap_or(0);
    let mut out = Vec::with_capacity(samples * n.max(1));
    for s in 0..samples {
        for p in &planes {
            out.push(p.get(s).copied().unwrap_or(0.0));
        }
    }
    out
}

/// duration_us, fps, width, height for a media file.
pub fn probe(path: &str) -> Result<(i64, f64, u32, u32)> {
    let ictx = ffmpeg::format::input(&path).context("probe")?;
    let duration_us = ictx.duration().max(0); // AV_TIME_BASE units == microseconds
    let mut fps = 0.0;
    let mut w = 0;
    let mut h = 0;
    for s in ictx.streams() {
        if s.parameters().medium() == Type::Video {
            let rate = s.avg_frame_rate();
            if rate.0 > 0 && rate.1 > 0 {
                fps = rate.0 as f64 / rate.1 as f64;
            }
            if let Ok(ctx) = ffmpeg::codec::context::Context::from_parameters(s.parameters()) {
                if let Ok(v) = ctx.decoder().video() {
                    w = v.width();
                    h = v.height();
                }
            }
            break;
        }
    }
    Ok((duration_us, fps, w, h))
}

pub fn has_audio_stream(path: &str) -> Result<bool> {
    let ictx = ffmpeg::format::input(&path).context("probe audio")?;
    Ok(ictx.streams().best(Type::Audio).is_some())
}

pub fn has_video_stream(path: &str) -> Result<bool> {
    let ictx = ffmpeg::format::input(&path).context("probe video")?;
    Ok(ictx.streams().best(Type::Video).is_some())
}

/// Downsampled min/max peaks (bins) from interleaved PCM for waveform display.
pub fn compute_peaks(pcm: &[f32], channels: u32, bins: usize) -> Vec<(f32, f32)> {
    let step = channels.max(1) as usize;
    let n = pcm.len() / step;
    if n == 0 || bins == 0 {
        return Vec::new();
    }
    let per = (n as f32 / bins as f32).ceil().max(1.0) as usize;
    let mut out = Vec::with_capacity(bins);
    let mut i = 0;
    while i < n {
        let hi = (i + per).min(n);
        let mut max = 0.0f32;
        let mut min = 0.0f32;
        for s in (i..hi).map(|x| x * step) {
            let v = pcm[s];
            if v > max {
                max = v;
            }
            if v < min {
                min = v;
            }
        }
        out.push((min, max));
        i = hi;
    }
    out
}

pub fn ensure_ffmpeg() {
    use std::sync::Once;
    static START: Once = Once::new();
    START.call_once(|| {
        let _ = ffmpeg::init();
    });
}

/// Load a still image into RGBA8.
pub fn load_image_rgba(path: &str) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::open(Path::new(path)).context("open image")?;
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Decode one (near-instant, aspect-squashed to `tw`x`th`) thumbnail frame.
/// Returns (w, h, rgba8). `None` when the file has no decodable first frame.
pub fn video_thumbnail(path: &str, tw: u32, th: u32) -> Result<(u32, u32, Vec<u8>)> {
    let mut src = VideoSource::open(path, tw.max(1), th.max(1))?;
    match src.frame_at(0) {
        Ok(Some(f)) => {
            let w = f.width.max(1);
            let h = f.height.max(1);
            let n = w as usize * h as usize * 4;
            Ok((w, h, f.rgba[..n.min(f.rgba.len())].to_vec()))
        }
        Ok(None) => bail!("no decodable frame in {path}"),
        Err(e) => Err(e),
    }
}

/// Bilinear downsample of an RGBA8 image into `tw`x`th` (keeps caller's box).
pub fn scale_rgba(rgba: &[u8], w: u32, h: u32, tw: u32, th: u32) -> Vec<u8> {
    let (w, h, tw, th) = (w.max(1), h.max(1), tw.max(1), th.max(1));
    let sx = w as f32 / tw as f32;
    let sy = h as f32 / th as f32;
    let mut out = vec![0u8; (tw as usize) * (th as usize) * 4];
    for y in 0..th {
        for x in 0..tw {
            let src_x = (x as f32 + 0.5) * sx - 0.5;
            let src_y = (y as f32 + 0.5) * sy - 0.5;
            let x0 = src_x.max(0.0) as u32;
            let y0 = src_y.max(0.0) as u32;
            let x1 = (x0 + 1).min(w - 1);
            let y1 = (y0 + 1).min(h - 1);
            let fx = (src_x - x0 as f32).clamp(0.0, 1.0);
            let fy = (src_y - y0 as f32).clamp(0.0, 1.0);
            let at = |u: u32, v: u32| -> [f32; 4] {
                let i = ((v * w + u) * 4) as usize;
                [
                    rgba.get(i).copied().unwrap_or(0) as f32,
                    rgba.get(i + 1).copied().unwrap_or(0) as f32,
                    rgba.get(i + 2).copied().unwrap_or(0) as f32,
                    rgba.get(i + 3).copied().unwrap_or(0) as f32,
                ]
            };
            let tl = at(x0, y0);
            let tr = at(x1, y0);
            let bl = at(x0, y1);
            let br = at(x1, y1);
            let lerp = |a: f32, b: f32, c: f32, d: f32| (a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy).clamp(0.0, 255.0);
            let o = ((y * tw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = lerp(tl[c], tr[c], bl[c], br[c]) as u8;
            }
        }
    }
    out
}