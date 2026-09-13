# rsedit — Technical Spec (v0.1 draft)

A minimal, fast, cross-platform video editor. Not a Resolve replacement — a
lightweight cutting tool with basic audio, image/transparency, and subtitle
support, built to never lag on the timeline.

## Goals / non-goals

**Goals**
- Instant playback and scrubbing, even on long timelines
- Fast launch, minimal UI chrome, low idle resource usage
- Cut/trim/arrange clips across video + audio + overlay tracks
- Basic audio mixing (volume, fades, multiple music/SFX tracks)
- Image overlays with alpha transparency (PNG/WebP/etc.)
- Text/subtitle rendering, burned in or exported as a sidecar file
- Cross-platform (Windows/macOS/Linux) from a single codebase

**Non-goals (v1)**
- Color grading (scopes, wheels, node-based color)
- Node-based compositing / VFX (Fusion-style)
- Multicam sync, collaborative editing, cloud project sync
- Full pro audio mixing console (sends, buses, plugin racks)

## Tech stack

| Layer | Choice | Notes |
|---|---|---|
| Core engine | Rust | No GC pauses, safe concurrency for the cache/decode threads |
| Decode/mux | FFmpeg (via bindings) | Video, audio, and image container/codec support |
| GPU compositing | wgpu | Vulkan/Metal/DX12 backends from one codebase |
| Audio engine | `cpal` (I/O) + `symphonia` or `rodio`-level mixing, custom mixer graph | Sample-accurate mixing, resampling for mismatched rates |
| Text rendering | `cosmic-text` or `rusttype` + a rasterizer to GPU texture | Subtitle burn-in and on-canvas text overlays |
| UI | egui or Slint | Native, no Electron/web overhead |
| Timeline data model | Custom struct graph, `serde`-serializable | Enables undo/redo, autosave |
| Subtitle formats | SRT / VTT import-export | Sidecar file support alongside burned-in text |

## Architecture

```
UI layer (egui/Slint)
        |
        v
Core engine (Rust)
  ├── Timeline model (clips, tracks, undo)
  ├── Frame cache (proxies, LRU, background pre-decode)
  ├── Audio mixer graph (per-track gain, fades, resampling)
  ├── Text/subtitle renderer (rasterize to texture)
  └── GPU compositor (wgpu) — composites video + image + text layers
        |
        v
FFmpeg (decode/encode) <-> Media files (video, audio, image)
        |
        v
Delivery / export (mux to output container)
```

Golden rule: **the UI/playback thread never blocks on decode.** All decode,
resample, and text rasterization happen on background thread pools; the
compositor only ever touches pre-cached frames/buffers.

## Timeline model

The timeline is a set of independent, time-aligned tracks:

- **Video tracks** (N) — video clips, transitions, image overlays
- **Audio tracks** (N) — music, SFX, dialogue; independent from video tracks
  so audio can be trimmed/faded without touching the picture edit
- **Text/subtitle track(s)** — timed text entries, rendered as an overlay
  layer during compositing, exportable to SRT/VTT independently of the
  burned-in render

Each track holds an ordered list of **clip instances**: a reference to a
source asset (video/audio/image/text), an in/out point into that source, a
position on the timeline, and a small set of per-clip properties (gain,
opacity, fade in/out, position/scale for overlays).

### Data model sketch

```rust
struct Project {
    video_tracks: Vec<Track<VideoClip>>,
    audio_tracks: Vec<Track<AudioClip>>,
    text_tracks: Vec<Track<TextClip>>,
    assets: HashMap<AssetId, Asset>,
}

struct Track<T> {
    clips: Vec<T>,
    muted: bool,
    locked: bool,
}

struct VideoClip {
    asset: AssetId,
    source_in: Timecode,
    source_out: Timecode,
    timeline_start: Timecode,
    opacity: f32,          // for image/video overlays with transparency
    transform: Transform,  // position/scale, for image overlays
}

struct AudioClip {
    asset: AssetId,
    source_in: Timecode,
    source_out: Timecode,
    timeline_start: Timecode,
    gain_db: f32,
    fade_in: Duration,
    fade_out: Duration,
}

struct TextClip {
    content: String,
    timeline_start: Timecode,
    timeline_end: Timecode,
    style: TextStyle, // font, size, color, position, outline/shadow
}
```

Undo/redo works as a command stack over discrete edits to this structure
(insert clip, trim clip, move clip, change property) — never a full-state
snapshot per action.

## Audio subsystem

- Audio lives on its own timeline, independent of video tracks, so a song
  can run under multiple video cuts without being split by them.
- Each audio track: per-clip gain, linear or equal-power fade in/out,
  track-level mute/solo.
- Mixing: sum all active clips at the current playhead sample position into
  a single output buffer per output channel, apply per-track gain, then
  master gain. Resample any clip whose source sample rate differs from the
  project rate before mixing.
- Waveform display: generate a downsampled peak cache per audio asset on
  import (background thread), so the timeline can draw waveforms instantly
  without re-reading the file.
- v1 scope: no sends/buses/plugin chains — just gain, fades, mute/solo per
  track. This is enough for "add a song under my cut and duck it a bit."

## Image & transparency support

- Import static images (PNG, WebP, TIFF) as timeline assets on video
  tracks, same as video clips but with a single "frame" held for the clip's
  duration.
- Alpha channel is preserved end-to-end: decoded to an RGBA texture,
  composited with straight/premultiplied alpha (pick one convention and
  apply consistently — premultiplied is friendlier for GPU blending) over
  whatever is below it in the track stack.
- Per-clip transform (position, scale, rotation) and opacity, so images can
  be used as logos, watermarks, or overlay graphics.

## Text & subtitles

- Text clips render to a GPU texture via the text rendering library, then
  composite as a top layer, same pipeline as image overlays.
- Two use modes:
  1. **Burned-in overlays** — styled text (font, size, color, outline,
     position) authored directly on a text track, exported into the video.
  2. **Subtitle track** — timed caption entries, either burned in at export
     or exported as a standalone SRT/VTT sidecar file alongside the video.
- Import existing SRT/VTT files directly onto a text track for editing.

## Rendering pipeline (per frame)

1. Determine active clips at the current timeline position across all
   video, image, and text tracks.
2. Fetch each clip's frame from the frame cache (or trigger async decode on
   miss, showing the nearest cached frame in the meantime rather than
   blocking).
3. Composite layers bottom-to-top on the GPU: video/image layers by
   opacity + transform, text/subtitle layer on top.
4. Present composited frame; independently, pull the audio mixer's buffer
   for the corresponding time range to the audio output.

## Export

- Walk the timeline frame-by-frame (video) and sample-by-sample (audio),
  running frames through the same compositor used for preview, encode via
  FFmpeg to the target container/codec.
- Export subtitle track separately as SRT/VTT if requested, in addition to
  or instead of burning it in.

## MVP build order

1. Media import + playback (single video, no timeline) — prove the core
   decode → cache → GPU → present loop is smooth.
2. Basic video timeline — single track, place/trim/split clips, scrub.
3. Multi-track video + image overlays with transparency.
4. Audio timeline — import music/SFX, gain, fades, mute/solo, waveform
   display, mixed playback in sync with video.
5. Text/subtitle track — on-canvas text overlays + SRT/VTT import/export.
6. Export — full timeline (video + audio + text) to a single output file.
7. Polish — fast launch, minimal UI, keyboard-first workflow.

## Open questions to resolve before coding

- Premultiplied vs straight alpha convention for the whole pipeline
- Target audio sample rate/format for internal mixing (e.g. always resample
  to 48kHz f32 internally)
- Project file format (custom binary vs JSON/RON) and versioning strategy
  for backward compatibility as the schema evolves
- Proxy resolution strategy (fixed downscale vs adaptive based on playback
  performance)