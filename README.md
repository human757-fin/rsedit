# Fast Cutter

A minimal, fast, cross-platform video editor. Not a Resolve replacement — a
lightweight cutting tool with basic audio, image/transparency, and subtitle
support, built to never lag on the timeline.

**Status: v0.1.1 — app shell + Windows/Linux release pipeline.**

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
| Decode/mux       | FFmpeg (via bindings)                         |
| GPU compositing  | wgpu (Vulkan/Metal/DX12)                      |
| Audio engine     | `cpal` + custom mixer graph, `symphonia`      |
| Text rendering   | `cosmic-text` / `rusttype`                    |
| UI               | egui or Slint                                 |
| Timeline model   | Custom struct graph, `serde`-serializable     |
| Subtitle formats | SRT / VTT import-export                       |

Golden rule: **the UI/playback thread never blocks on decode.** All decode,
resample, and text rasterization happen on background thread pools.

## Roadmap (MVP build order)

1. Media import + playback (single video, no timeline)
2. Basic video timeline — single track, place/trim/split, scrub
3. Multi-track video + image overlays with transparency
4. Audio timeline — music/SFX, gain, fades, mute/solo, waveforms
5. Text/subtitle track — overlays + SRT/VTT import/export
6. Export — full timeline to a single output file
7. Polish — fast launch, minimal UI, keyboard-first workflow

## Spec

The full technical specification lives in [`rsedit.md`](rsedit.md).

## Building

```sh
cargo build --release        # desktop app (egui)
cargo run                    # run the app
```

## Windows installer & releases

Every tag pushed as `v*` (or a manual workflow run) builds on GitHub Actions
and publishes a **Release** with these artifacts:

| File | What it is |
|---|---|
| `FastCutter-Setup-*.exe` | A small bootstrap installer (`installer/`). It downloads the latest portable build straight from GitHub Releases **on the user's machine**, installs per-user to `%LOCALAPPDATA%\Programs\FastCutter` (no admin needed), creates a Start Menu shortcut, and registers an Uninstall entry. |
| `FastCutter-*-windows-portable.zip` | Complete Windows portable build: `FastCutter.exe` + docs. Download → unzip → run. |
| `FastCutter-*-linux-x86_64.tar.gz` | Complete Linux portable build: `FastCutter` binary + docs. Extract → run. |
| `FastCutter-*-x86_64.AppImage` | Linux AppImage. Download, `chmod +x`, run — no install required. |

`SHA256SUMS.txt` is published alongside every release for verification.

Want a release? Push a tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

Or run the `release` workflow manually from the Actions tab.

## License

MIT — see [LICENSE](LICENSE).