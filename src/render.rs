//! CPU compositor: scale + alpha blend video/image clips and rasterize text
//! clips into one RGBA8 composition buffer. Shared by the preview viewport and
//! by the exporter via `compose_frame`, so WYSIWYG holds by construction.

use ab_glyph::{Font, FontArc, PxScale, PxScaleFont, ScaleFont};
use std::sync::Arc;

use crate::decoder::VideoFrame;
use crate::framecache::FrameCache;
use crate::timeline::{
    AssetKind, BlendMode, ColorGrade, Project, TextAlign, TextStyle, Transform, VideoClip,
};

const BG: [u8; 4] = [8, 8, 10, 255]; // near-black preview background

/// Render a full composition frame through an abstract frame provider.
///
/// `fetch(asset_id, path, source_pts, up_to)` returns the decoded (or
/// synthesized) source frame to composite; this indirection lets the preview
/// use `FrameCache` and the exporter use its own decoder pool.
pub fn compose_frame(
    project: &Project,
    w: u32,
    h: u32,
    pts_us: i64,
    mut fetch: impl FnMut(u64, &str, i64, i64) -> Option<Arc<VideoFrame>>,
) -> Vec<u8> {
    let mut out: Vec<u8> = vec![0; (w * h * 4) as usize];
    clear_buffer(&mut out);

    for track in &project.video_tracks {
        let n = track.clips.len();
        for (i, clip) in track.clips.iter().enumerate() {
            let Some(asset) = project.assets.get(clip.asset) else { continue };
            let start = clip.timeline_start;
            let duration = clip.on_timeline_us().max(0);
            if pts_us < start || pts_us >= start + duration {
                continue;
            }
            let local = pts_us - start;
            let source_pts = clip.source_at(local);

            // Crossfade alpha: fade in from the previous clip's tail, and fade
            // out toward the next clip that opens with a transition.
            let mut alpha = clip.opacity as f32;
            let tin = clip.transition_us.max(0) as f32;
            if tin > 0.0 && local < tin as i64 {
                alpha *= (local as f32 / tin).clamp(0.0, 1.0);
            }
            if i + 1 < n {
                let next = &track.clips[i + 1];
                let tn = next.transition_us.max(0) as i64;
                let end = start + duration;
                if tn > 0 && next.timeline_start == end && pts_us >= end - tn {
                    let rem = (end - pts_us) as f32 / tn as f32;
                    alpha *= rem.clamp(0.0, 1.0);
                }
            }
            if alpha < 0.001 {
                continue;
            }

            let frame = match asset.kind {
                AssetKind::Image => asset.rgba.as_ref().map(|rgba| {
                    Arc::new(VideoFrame {
                        pts_us: source_pts,
                        width: asset.width,
                        height: asset.height,
                        rgba: rgba.clone(),
                    })
                }),
                _ => fetch(asset.id.0, &asset.path, source_pts, clip.source_out),
            };
            if let Some(f) = frame {
                let tf = effective_transform(clip, local);
                blend_scaled(
                    &f.rgba,
                    f.width,
                    f.height,
                    &mut out,
                    w,
                    h,
                    &tf,
                    alpha,
                    &clip.grade,
                    clip.flip_x,
                    clip.flip_y,
                    &clip.crop,
                    clip.blend,
                );
            }
        }
    }

    // Text tracks (topmost in preview order). Text stays fully visible for its
    // whole window; visual stacking is just track/clip order.
    for track in &project.text_tracks {
        for clip in &track.clips {
            let start = clip.timeline_start;
            let duration = (clip.timeline_end - clip.timeline_start).max(0);
            if pts_us < start || pts_us >= start + duration {
                continue;
            }
            let active = crate::text::text_to_subtitle(&clip.text, start, duration);
            if pts_us >= active.start_us && pts_us < active.end_us {
                let reveal = if clip.style.typewriter {
                    ((pts_us - start) as f32 / duration.max(1) as f32).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                draw_text_billboard(
                    &mut out,
                    w,
                    h,
                    &active.text,
                    &clip.style,
                    clip.transform,
                    clip.opacity,
                    reveal,
                );
            }
        }
    }
    out
}

/// Effective transform at `local_us`: interpolates keyframes when present,
/// otherwise returns the clip's static transform.
pub fn effective_transform(clip: &VideoClip, local_us: i64) -> Transform {
    let kf = &clip.keyframes;
    if kf.is_empty() {
        return clip.transform;
    }
    let tl = local_us as f64;
    if tl <= kf[0].t_us as f64 {
        return kf[0].transform;
    }
    let last = &kf[kf.len() - 1];
    if tl >= last.t_us as f64 {
        return last.transform;
    }
    for w in kf.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if tl >= a.t_us as f64 && tl <= b.t_us as f64 {
            let f = ((tl - a.t_us as f64) / (b.t_us - a.t_us).max(1) as f64) as f32;
            return Transform {
                x: a.transform.x + (b.transform.x - a.transform.x) * f,
                y: a.transform.y + (b.transform.y - a.transform.y) * f,
                scale: a.transform.scale + (b.transform.scale - a.transform.scale) * f,
                rotate: a.transform.rotate + (b.transform.rotate - a.transform.rotate) * f,
            };
        }
    }
    clip.transform
}

/// Fit + center the source into the composition (respecting transform),
/// applying crop / flip / color grade, bilinear sample inside the destination
/// rect and alpha-blend by `opacity` with `mode`.
#[allow(clippy::too_many_arguments)]
pub fn blend_scaled(
    src: &[u8],
    sw: u32,
    sh: u32,
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    transform: &Transform,
    opacity: f32,
    grade: &ColorGrade,
    flip_x: bool,
    flip_y: bool,
    crop: &[f32; 4],
    mode: BlendMode,
) {
    if src.len() < (sw * sh * 4) as usize {
        return;
    }
    let scale = transform.scale.max(0.001);
    let cl = crop[0].clamp(0.0, 1.0);
    let ct = crop[1].clamp(0.0, 1.0);
    let cr = crop[2].clamp(cl, 1.0);
    let cb = crop[3].clamp(ct, 1.0);
    if cr - cl < 1e-4 || cb - ct < 1e-4 {
        return;
    }
    let cw = (sw as f32 * (cr - cl)) as i32;
    let ch = (sh as f32 * (cb - ct)) as i32;
    let fit = ((dw as f32 / cw.max(1) as f32).min(dh as f32 / ch.max(1) as f32)) * scale;
    let out_w = (cw as f32 * fit) as i32;
    let out_h = (ch as f32 * fit) as i32;
    let cx = (dw as f32 * transform.x) as i32;
    let cy = (dh as f32 * transform.y) as i32;
    let rot = transform.rotate.to_radians();

    let w2 = out_w / 2;
    let h2 = out_h / 2;
    let cos = rot.cos();
    let sin = rot.sin();
    let rx = (w2.abs() as f32 * cos.abs() + h2.abs() as f32 * sin.abs()).abs() as i32;
    let ry = (h2.abs() as f32 * cos.abs() + w2.abs() as f32 * sin.abs()).abs() as i32;

    let has_hue = grade.hue_shift.abs() > 0.001;

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
            let u = (dx as f32 * cos + dy as f32 * sin) / fit;
            let v = (-dx as f32 * sin + dy as f32 * cos) / fit;
            let mut fu = (u + 0.5) * cw as f32;
            let mut fv = (v + 0.5) * ch as f32;
            if flip_x {
                fu = cw as f32 - fu - 1.0;
            }
            if flip_y {
                fv = ch as f32 - fv - 1.0;
            }
            let su = (fu + cl * sw as f32) as i32;
            let sv = (fv + ct * sh as f32) as i32;
            if su < 0 || sv < 0 || su >= sw as i32 || sv >= sh as i32 {
                continue;
            }
            let si = ((sv as usize) * (sw as usize) + su as usize) * 4;
            let mut r = src[si] as f32;
            let mut g = src[si + 1] as f32;
            let mut b = src[si + 2] as f32;
            if grade.brightness != 0.0 || grade.contrast != 1.0 || grade.saturation != 1.0 {
                r = apply_grade(r, grade, 0);
                g = apply_grade(g, grade, 1);
                b = apply_grade(b, grade, 2);
                if grade.saturation != 1.0 {
                    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                    let m = grade.saturation;
                    r = luma + (r - luma) * m;
                    g = luma + (g - luma) * m;
                    b = luma + (b - luma) * m;
                }
            }
            if has_hue {
                let (hval, sval, vval) = rgb_to_hsv(r, g, b);
                let (rr, gg, bb) = hsv_to_rgb((hval + grade.hue_shift).rem_euclid(std::f32::consts::TAU), sval, vval);
                r = rr;
                g = gg;
                b = bb;
            }
            let a = (src[si + 3] as f32 / 255.0) * opacity.min(1.0);
            let di = (y as usize) * (dw as usize) * 4 + (x as usize) * 4;
            let srcc = [r.min(255.0), g.min(255.0), b.min(255.0), 255.0];
            blend_pixel(dst, di, &srcc, a, mode);
        }
    }
}

#[inline]
fn apply_grade(c: f32, grade: &ColorGrade, _chan: usize) -> f32 {
    let mut c = c + grade.brightness * 255.0;
    if grade.contrast != 1.0 {
        c = (c / 255.0 - 0.5) * grade.contrast * 255.0 + 128.0;
    }
    c
}

#[inline]
fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let r = r / 255.0;
    let g = g / 255.0;
    let b = b / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let mut h = 0.0;
    if d > 0.0 {
        if max == r {
            h = ((g - b) / d).rem_euclid(6.0);
        } else if max == g {
            h = (b - r) / d + 2.0;
        } else {
            h = (r - g) / d + 4.0;
        }
        h *= std::f32::consts::FRAC_PI_3;
    }
    let s = if max > 0.0 { d / max } else { 0.0 };
    (h, s, max)
}

#[inline]
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let i = (h / std::f32::consts::FRAC_PI_3).floor() as i32;
    let f = h / std::f32::consts::FRAC_PI_3 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (r * 255.0, g * 255.0, b * 255.0)
}

/// Blend `srcc` (premultiplied-by-alpha handled via `a`) into the destination
/// with the given blend mode. The mode result is mixed with dst by `a`.
#[inline]
fn blend_pixel(dst: &mut [u8], di: usize, srcc: &[f32; 4], a: f32, mode: BlendMode) {
    if a <= 0.0 {
        return;
    }
    let d = [dst[di] as f32, dst[di + 1] as f32, dst[di + 2] as f32];
    let blended = match mode {
        BlendMode::Normal => [srcc[0], srcc[1], srcc[2]],
        BlendMode::Multiply => [
            srcc[0] * d[0] / 255.0,
            srcc[1] * d[1] / 255.0,
            srcc[2] * d[2] / 255.0,
        ],
        BlendMode::Screen => [
            255.0 - (255.0 - srcc[0]) * (255.0 - d[0]) / 255.0,
            255.0 - (255.0 - srcc[1]) * (255.0 - d[1]) / 255.0,
            255.0 - (255.0 - srcc[2]) * (255.0 - d[2]) / 255.0,
        ],
        BlendMode::Add => [
            (srcc[0] + d[0]).min(255.0),
            (srcc[1] + d[1]).min(255.0),
            (srcc[2] + d[2]).min(255.0),
        ],
        BlendMode::Overlay => [
            overlay_ch(srcc[0], d[0]),
            overlay_ch(srcc[1], d[1]),
            overlay_ch(srcc[2], d[2]),
        ],
    };
    let ia = 1.0 - a;
    dst[di] = (blended[0] * a + d[0] * ia).round() as u8;
    dst[di + 1] = (blended[1] * a + d[1] * ia).round() as u8;
    dst[di + 2] = (blended[2] * a + d[2] * ia).round() as u8;
    dst[di + 3] = 255;
}

#[inline]
fn overlay_ch(s: f32, d: f32) -> f32 {
    if d < 128.0 {
        2.0 * s * d / 255.0
    } else {
        255.0 - 2.0 * (255.0 - s) * (255.0 - d) / 255.0
    }
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
    reveal: f32,
) {
    let Some(loaded) = crate::fonts::load_font(&style.font_family) else { return };
    let font: FontArc = loaded.font;
    let size = style.font_size.max(1.0);
    let scale = PxScale::from(size);
    let scaled = font.as_scaled(scale);

    // Wrap into lines.
    let raw: Vec<&str> = text.split('\n').collect();
    let mut lines: Vec<String> = Vec::new();
    for l in raw {
        if style.word_wrap > 0.0 {
            wrap_line(&scaled, l, style.word_wrap, &mut lines);
        } else {
            lines.push(l.to_string());
        }
    }
    let line_gap = size * 0.25;
    let widths: Vec<f32> = lines.iter().map(|l| measure(&scaled, l)).collect();
    let total_w = widths.iter().cloned().fold(0.0f32, f32::max).max(1.0);
    let total_h =
        scaled.height() * lines.len() as f32 + line_gap * lines.len().saturating_sub(1) as f32;
    let align = style.align;

    let cx = dw as f32 * transform.x;
    let cy = dh as f32 * transform.y;
    let rot = transform.rotate.to_radians();
    let (cos, sin) = (rot.cos(), rot.sin());

    // Background box behind the whole block.
    if style.background[3] > 0 {
        let pad = style.box_padding.max(0.0);
        fill_rect(
            dst,
            dw,
            dh,
            cx - total_w / 2.0 - pad,
            cy - total_h / 2.0 - pad,
            total_w + pad * 2.0,
            total_h + pad * 2.0,
            rot,
            style.background,
            opacity,
        );
    }

    let base_y = cy - total_h / 2.0 + scaled.ascent() + size * 0.5;
    let total_chars: usize = lines.iter().map(|l| l.chars().count()).sum();
    let vis = if style.typewriter {
        ((total_chars as f32 * reveal.clamp(0.0, 1.0)) as usize).min(total_chars)
    } else {
        total_chars
    };
    let mut chars_done = 0usize;

    let mut y = base_y;
    for (i, l) in lines.iter().enumerate() {
        let w = widths[i];
        let x0 = cx - total_w / 2.0 + (total_w - w) * align_x(align);
        let mut pen = x0;
        for c in l.chars() {
            let draw_this = chars_done < vis;
            chars_done += 1;
            if !draw_this {
                pen += scaled.h_advance(font.glyph_id(c));
                continue;
            }
            let glyph = font.glyph_id(c).with_scale_and_position(scale, ab_glyph::point(pen, y));
            if let Some(og) = font.outline_glyph(glyph) {
                if style.shadow {
                    emit_glyph(dst, dw, dh, cx, cy, cos, sin, &og, 2.0, 2.0, [0, 0, 0, 255], 0.35, style, opacity);
                }
                if style.outline_width > 0.001 && style.outline_color[3] > 0 {
                    let ow = style.outline_width;
                    for (ox, oy) in [
                        (ow, 0.0),
                        (-ow, 0.0),
                        (0.0, ow),
                        (0.0, -ow),
                        (ow * 0.7, ow * 0.7),
                        (-ow * 0.7, ow * 0.7),
                        (ow * 0.7, -ow * 0.7),
                        (-ow * 0.7, -ow * 0.7),
                    ] {
                        emit_glyph(
                            dst, dw, dh, cx, cy, cos, sin, &og, ox, oy,
                            [style.outline_color[0], style.outline_color[1], style.outline_color[2], 255],
                            1.0,
                            style,
                            opacity,
                        );
                    }
                }
                emit_glyph(
                    dst, dw, dh, cx, cy, cos, sin, &og, 0.0, 0.0, style.color, 1.0, style, opacity,
                );
            }
            pen += scaled.h_advance(font.glyph_id(c));
        }
        y += scaled.height() + line_gap;
    }
}

fn emit_glyph(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    cx: f32,
    cy: f32,
    cos: f32,
    sin: f32,
    og: &ab_glyph::OutlinedGlyph,
    ox: f32,
    oy: f32,
    color: [u8; 4],
    color_mult: f32,
    style: &TextStyle,
    opacity: f32,
) {
    let bounds = og.px_bounds();
    og.draw(|px, py, cov| {
        let gx = bounds.min.x + px as f32 + ox;
        let gy = bounds.min.y + py as f32 + oy;
        let rx = gx * cos - gy * sin + cx * (1.0 - cos) + cy * sin;
        let ry = gx * sin + gy * cos + cy * (1.0 - cos) - cx * sin;
        let px_i = rx.round() as i32;
        let py_i = ry.round() as i32;
        if px_i < 0 || py_i < 0 || px_i >= dw as i32 || py_i >= dh as i32 {
            return;
        }
        let a = cov.min(1.0) * (color[3] as f32 / 255.0) * (style.color[3] as f32 / 255.0)
            * color_mult
            * opacity.min(1.0);
        if a <= 0.01 {
            return;
        }
        let di = (py_i as usize) * (dw as usize) * 4 + (px_i as usize) * 4;
        blend_pixel(dst, di, &[color[0] as f32, color[1] as f32, color[2] as f32, 255.0], a.clamp(0.0, 1.0), BlendMode::Normal);
    });
}

/// Render `text` on `vals` (0..1) lines computed by the wrap helper.
fn wrap_line(
    scaled: &PxScaleFont<&FontArc>,
    line: &str,
    max_w: f32,
    out: &mut Vec<String>,
) {
    let mut cur = String::new();
    let mut cur_w = 0.0f32;
    for word in line.split_whitespace() {
        let ww = measure(scaled, word);
        let sep = if cur.is_empty() { 0.0 } else { measure(scaled, " ") };
        if cur_w + sep + ww > max_w && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur_w = 0.0;
        }
        if !cur.is_empty() {
            cur.push(' ');
            cur_w += sep;
        }
        cur.push_str(word);
        cur_w += ww;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
}

fn measure(scaled: &PxScaleFont<&FontArc>, s: &str) -> f32 {
    let mut w = 0.0f32;
    for c in s.chars() {
        w += scaled.h_advance(scaled.font().glyph_id(c));
    }
    w
}

/// Fill a rotated rectangle (used for the text background box).
fn fill_rect(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    x0: f32,
    y0: f32,
    w: f32,
    h: f32,
    rot: f32,
    color: [u8; 4],
    opacity: f32,
) {
    let (cos, sin) = (rot.cos(), rot.sin());
    let cx = x0 + w / 2.0;
    let cy = y0 + h / 2.0;
    // Bounding box of rotated rect.
    let hw = w / 2.0;
    let hh = h / 2.0;
    let rx = hw * cos.abs() + hh * sin.abs();
    let ry = hh * cos.abs() + hw * sin.abs();
    let a = (color[3] as f32 / 255.0) * opacity.min(1.0);
    if a <= 0.01 {
        return;
    }
    for py in ((cy - ry) as i32)..((cy + ry) as i32) {
        if py < 0 || py >= dh as i32 {
            continue;
        }
        for px in ((cx - rx) as i32)..((cx + rx) as i32) {
            if px < 0 || px >= dw as i32 {
                continue;
            }
            let dx = px as f32 - cx;
            let dy = py as f32 - cy;
            let lx = dx * cos + dy * sin;
            let ly = -dx * sin + dy * cos;
            if lx.abs() <= hw && ly.abs() <= hh {
                let di = (py as usize) * (dw as usize) * 4 + (px as usize) * 4;
                blend_pixel(
                    dst,
                    di,
                    &[
                        color[0] as f32,
                        color[1] as f32,
                        color[2] as f32,
                        255.0,
                    ],
                    a,
                    BlendMode::Normal,
                );
            }
        }
    }
}

fn align_x(a: TextAlign) -> f32 {
    match a {
        TextAlign::Left => 0.0,
        TextAlign::Center => 0.5,
        TextAlign::Right => 1.0,
    }
}

/// Composition wrapper used by the preview.
pub struct Composition<'a> {
    pub project: &'a Project,
    pub cache: &'a FrameCache,
    pub w: u32,
    pub h: u32,
}

impl<'a> Composition<'a> {
    pub fn render_at(&self, pts_us: i64) -> Vec<u8> {
        compose_frame(self.project, self.w, self.h, pts_us, |asset, _path, src, up_to| {
            self.cache.nearest(asset, src, Some(up_to.max(src)))
        })
    }
}