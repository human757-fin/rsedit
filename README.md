# Fast Cutter

A minimal, fast, cross-platform video editor. Not a Resolve replacement — a
lightweight cutting tool with basic audio, image/transparency, and subtitle
support, built to never lag on the timeline.

**Status: v0.2.0 — full MVP: timeline, import, playback, text, audio mixing, export.**

## Features (v0.2.0)

- **Media import** — drag-and-drop or browse to load video (MP4/MOV/WebM),
  audio (MP3/WAV/AAC/FLAC), and images (PNG/JPEG/WebP).
- **Multi-track timeline** — video, audio, and text tracks with trim, split,
  mute, and drag-to-rearrange.
- **Live preview** — hardware-accelerated playback at full frame rate via a
  background decode thread; the UI never blocks.
- **Text overlays** — sized, positioned, coloured text with left/center/right
  alignment; system fonts via `fontdb`.
- **Audio mixing** — per-clip gain (dB), linear fades, and per-track mute,
  mixed at 48 kHz internally with zero allocation on the audio callback.
- **Export** — H.264 + AAC to MP4, WYSIWYG through the same compositor used
  for preview.

## Goals

- Instant playback and scrubbing, even on long timelines
- Fast launch, minimal UI chrome, low idle resource usage
- Cut/trim/arrange clips across video + audio + overlay tracks
- Basic audio mixing (volume, fades, multiple music/SFX tracks)
- Image overlays with alpha transparency (PNG/WebP/etc.)
- Text/subtitle rendering, burned in or exported as a sidecar file
- Cross-platform (Windows/macOS/Linux) from a single codebase

## Non-goals (v1)

- Color grading (scopes, wheels, node-based color)
- Node-based compositing / VFX
- Multicam sync, collaborative editing, cloud project sync
- Full pro audio mixing console (sends, buses, plugin racks)

## Tech stack

| Layer            | Choice                                        |
|------------------|-----------------------------------------------|
| Core engine      | Rust                                          |
| Decode/mux       | FFmpeg (`ffmpeg-next` v7 bindings)            |
| Compositing      | CPU bilinear scaler + alpha blend (WYSIWYG)   |
| Audio engine     | `cpal` + custom mixer graph                   |
| Text rendering   | `ab_glyph` + `fontdb`                        |
| UI               | egui (native desktop)                         |
| Timeline model   | Custom struct graph, `serde`-serializable     |
| Subtitle formats | SRT / VTT import                              |

Golden rule: **the UI/playback thread never blocks on decode.** All decode,
resample, and text rasterization happen on background threads.

## Spec

The full technical specification lives in [`rsedit.md`](rsedit.md).

## Building locally

### Prerequisites

Install the development headers for FFmpeg and ALSA (plus `libclang` for
bindgen, which is used by `ffmpeg-sys-next`):

```sh
# Ubuntu / Debian
sudo apt-get install -y \
  libavcodec-dev libavformat-dev libavutil-dev \
  libswscale-dev libavfilter-dev libswresample-dev \
  libasound2-dev clang pkg-config
```

### Build

```sh
cargo build --release        # release binary
cargo run                    # run the app
cargo test                   # run unit tests
```

## Windows installer & releases

Every tag pushed as `v*` (or a manual workflow run) builds on GitHub Actions
and publishes a **Release** with these artifacts:

| File | What it is |
|---|---|
| `FastCutter-Setup-*.exe` | A small bootstrap installer (`installer/`). It downloads the latest portable build straight from GitHub Releases **on the user's machine**, installs per-user to `%LOCALAPPDATA%\Programs\FastCutter` (no admin needed), creates a Start Menu shortcut, and registers an Uninstall entry. |
| `FastCutter-*-windows-portable.zip` | Windows portable build: `FastCutter.exe` + runtime FFmpeg DLLs + docs. Download → unzip → run. |
| `FastCutter-*-linux-x86_64.tar.gz` | Linux portable build. Requires runtime FFmpeg libs on the system (`libavcodec61` etc.). Extract → run. |
| `FastCutter-*-x86_64.AppImage` | Linux AppImage. Self-contained, runs on most distros without installing anything. |

`SHA256SUMS.txt` is published alongside every release for verification.

Want a release? Push a tag:

```sh
git tag v0.2.0
git push origin v0.2.0
```

Or run the `release` workflow manually from the Actions tab.

## License

MIT — see [LICENSE](LICENSE).