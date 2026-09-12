//! CPU compositor: scale + alpha blend video/image clips and rasterize text
//! clips into one RGBA8 composition buffer. Shared by the preview viewport and
//! by the exporter, so WYSIWYG holds by construction.

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use std::sync::Arc;

use crate::decoder::VideoFrame;
use crate::framecache::FrameCache;
use crate::timeline::{Project, TextStyle, TextAlign, Transform};

const BG: [u8; 4] = [8, 8, 10, 255]; // near-black preview background

pub struct Composition<'a> {
    pub project: &'a Project,
    pub cache: &'a FrameCache,
    pub w: u32,
    pub h: u32,
}

impl<'a> Composition<'a> {
    /// Render the composition at a given timeline position (µs).
    /// Returns an RGBA8 buffer of w*h*4.
    pub fn render_at(&self, pts_us: i64) -> Vec<u8> {
        let mut out: Vec<u8> = vec![0; (self.w * self.h * 4) as usize];
        // Background
        for px in out.chunks_exact_mut(4) {
            px.copy_from_slice(&BG);
        }

        let pts = pts_us;
        for track in &self.project.video_tracks {
            for clip in &track.clips {
                let Some(asset) = self.project.assets.get(clip.asset) else { continue };
                let start = clip.timeline_start;
                let duration = (clip.source_out - clip.source_in).max(0);
                if pts < start || pts >= start + duration {
                    continue;
                }
                let local = pts - start;
                let source_pts = clip.source_in + local;

                let frame: Option<Arc<VideoFrame>> = match asset.kind {
                    crate::timeline::AssetKind::Image => asset.rgba.as_ref().map(|rgba| {
                        Arc::new(VideoFrame {
                            pts_us: source_pts,
                            width: asset.width,
                            height: asset.height,
                            rgba: rgba.clone(),
                        })
                    }),
                    _ => self.cache.nearest(asset.id.0, source_pts, Some(clip.source_out)),
                };
                if let Some(f) = frame {
                    blend_scaled(
                        &f.rgba,
                        f.width,
                        f.height,
                        &mut out,
                        self.w,
                        self.h,
                        &clip.transform,
                        clip.opacity,
                    );
                }
            }
        }

        // Text tracks (topmost in preview order)
        for track in &self.project.text_tracks {
            for clip in &track.clips {
                let start = clip.timeline_start;
                let duration = (clip.timeline_end - clip.timeline_start).max(0);
                if pts < start || pts >= start + duration {
                    continue;
                }
                let active = crate::text::text_to_subtitle(&clip.text, start, duration);
                if pts >= active.start_us && pts < active.end_us {
                    draw_text_billboard(
                        &mut out,
                        self.w,
                        self.h,
                        &active.text,
                        &clip.style,
                        clip.transform,
                        clip.opacity,
                    );
                }
            }
        }
        out
    }
}

/// Fit + center the source into the composition (respecting transform), bilinear
/// sample inside the destination rect and alpha-blend by `opacity`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn blend_scaled(
    src: &[u8],
    sw: u32,
    sh: u32,
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    transform: &Transform,
    opacity: f32,
) {
    if src.len() < (sw * sh * 4) as usize {
        return;
    }
    let scale = transform.scale.max(0.001);
    let fit = ((dw as f32 / sw as f32).min(dh as f32 / sh as f32)) * scale;
    let out_w = (sw as f32 * fit) as i32;
    let out_h = (sh as f32 * fit) as i32;
    let cx = (dw as f32 * transform.x) as i32;
    let cy = (dh as f32 * transform.y) as i32;
    let rot = transform.rotate.to_radians();

    // We sample the dest rect of the rotated box (enlarged by the rotated
    // bbox) so corners are covered.
    let w2 = out_w / 2;
    let h2 = out_h / 2;
    let cos = rot.cos();
    let sin = rot.sin();
    let rx = (w2.abs() as f32 * cos.abs() + h2.abs() as f32 * sin.abs()).abs() as i32;
    let ry = (h2.abs() as f32 * cos.abs() + w2.abs() as f32 * sin.abs()).abs() as i32;

    for y in (cy - ry)..(cy + ry) {
        if y < 0 || y >= dh as i32 {
            continue;
        }
        for x in (cx - rx)..(cx + rx) {
            if x < 0 || x >= dw as i32 {
                continue;
            }
            let dx = x - cx;
            let dy = y - cy;
            // inverse rotate
            let u = (dx as f32 * cos + dy as f32 * sin) / fit;
            let v = (-dx as f32 * sin + dy as f32 * cos) / fit;
            let su = (u + sw as f32 / 2.0) as i32;
            let sv = (v + sh as f32 / 2.0) as i32;
            if su < 0 || sv < 0 || su >= sw as i32 || sv >= sh as i32 {
                continue;
            }
            let si = ((sv as usize) * (sw as usize) + su as usize) * 4;
            let a = (src[si + 3] as f32 / 255.0) * opacity.min(1.0);
            render_blend(dst, (y as usize) * (dw as usize) * 4 + (x as usize) * 4, &src[si..si + 4], a);
        }
    }
}

#[inline]
fn render_blend(dst: &mut [u8], di: usize, srcc: &[u8], a: f32) {
    if a <= 0.0 {
        return;
    }
    let ia = 1.0 - a;
    dst[di] = (srcc[0] as f32 * a + dst[di] as f32 * ia).round() as u8;
    dst[di + 1] = (srcc[1] as f32 * a + dst[di + 1] as f32 * ia).round() as u8;
    dst[di + 2] = (srcc[2] as f32 * a + dst[di + 2] as f32 * ia).round() as u8;
    dst[di + 3] = 255;
}

pub fn clear_buffer(out: &mut [u8]) {
    for px in out.chunks_exact_mut(4) {
        px.copy_from_slice(&BG);
    }
}

pub fn draw_text_billboard(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    text: &str,
    style: &TextStyle,
    transform: Transform,
    opacity: f32,
) {
    let Some(loaded) = crate::fonts::load_font(&style.font_family) else { return };
    let font: FontArc = loaded.font;
    let size = style.font_size.max(1.0);
    let scale = PxScale::from(size);
    let scaled = font.as_scaled(scale);
    let lines: Vec<&str> = text.lines().collect();
    let line_gap = size * 0.25;

    let mut widths = Vec::new();
    for l in &lines {
        let mut w = 0.0f32;
        for c in l.chars() {
            let g = font.glyph_id(c).with_scale_and_position(scale, ab_glyph::point(0.0, 0.0));
            w += font.outline_glyph(g).map_or(size * 0.5, |og| og.px_bounds().width());
        }
        widths.push(w);
    }
    let total_h = scaled.height() * lines.len() as f32 + line_gap * (lines.len().saturating_sub(1)) as f32;
    let total_w = widths.iter().cloned().fold(0.0f32, f32::max).max(1.0);
    let align = style.align;

    let cx = dw as f32 * transform.x;
    let cy = dh as f32 * transform.y;
    let rot = transform.rotate.to_radians();
    let (cos, sin) = (rot.cos(), rot.sin());

    let base_y = cy - total_h / 2.0 + scaled.ascent() + size * 0.5;
    let mut y = base_y;
    for (i, l) in lines.iter().enumerate() {
        let w = widths[i];
        let x0 = cx - total_w / 2.0 + (total_w - w) * align_x(align);
        for c in l.chars() {
            let g = font
                .glyph_id(c)
                .with_scale_and_position(scale, ab_glyph::point(x0, y));
            if let Some(og) = font.outline_glyph(g) {
                let bounds = og.px_bounds();
                if rot == 0.0 {
                    og.draw(|px, py, cov| {
                        let gx = bounds.min.x + px as f32;
                        let gy = bounds.min.y + py as f32;
                        blend_text_pixel(dst, dw, dh, gx, gy, cov, style, opacity);
                    });
                } else {
                    og.draw(|px, py, cov| {
                        let gx = bounds.min.x + px as f32;
                        let gy = bounds.min.y + py as f32;
                        let dx = gx - cx;
                        let dy = gy - cy;
                        let rx = dx * cos - dy * sin + cx;
                        let ry = dx * sin + dy * cos + cy;
                        blend_text_pixel(dst, dw, dh, rx, ry, cov, style, opacity);
                    });
                }
            }
        }
        y += scaled.height() + line_gap;
    }
}

#[inline]
fn blend_text_pixel(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    x: f32,
    y: f32,
    cov: f32,
    style: &TextStyle,
    opacity: f32,
) {
    let px = x.round() as i32;
    let py = y.round() as i32;
    if px < 0 || py < 0 || px >= dw as i32 || py >= dh as i32 {
        return;
    }
    let a = cov.min(1.0) * (style.color[3] as f32 / 255.0) * opacity.min(1.0);
    if a <= 0.01 {
        return;
    }
    let di = (py as usize) * (dw as usize) * 4 + (px as usize) * 4;
    render_blend(
        dst,
        di,
        &[style.color[0], style.color[1], style.color[2], 255],
        a.clamp(0.0, 1.0),
    );
}

fn align_x(a: TextAlign) -> f32 {
    match a {
        TextAlign::Left => 0.0,
        TextAlign::Center => 0.5,
        TextAlign::Right => 1.0,
    }
}