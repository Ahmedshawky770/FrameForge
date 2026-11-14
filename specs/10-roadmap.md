# Phased Roadmap & Acceptance Criteria

All phases are Linux-first. Each phase must pass clippy/fmt/tests before the next begins. No phase begins until the prior phase's acceptance criteria are met.

## P0 — Video-presentation spike + ADR
Goal: Answer "How do I show video inside a Tauri window on Pop!_OS?"
Work:
- Build minimal spike app (Tauri 2 + Strategy A: libmpv `wid` into dedicated X11 child window on X11/XWayland; Strategy B: native mpv window with gpu-next/wayland on Wayland).
- Measure on X11 and Wayland (GNOME + COSMIC): cold start, idle RAM, playback RAM, dropped frames, seek latency p50/p95, hwdec active, WebView input, Arabic RTL overlay.
- Finalize ADR-2 (strategy A / B / E) with measured data.
Acceptance:
- Video displays and plays on X11 and Wayland.
- Playback 1080p30 with ≥95% frames, hwdec active when available.
- WebView input events reach UI; Arabic RTL renders.
- ADR published with thresholds and fallback.
**Status: COMPLETE** (see `specs/03-phase0-spike-adr.md`)

## P1 — Core library (probe, keyframe index, decode frame, HW detect, diagnostics, CLI)
Work:
- `core-types`, `media-probe`, `scrub-engine`, `hw-diagnostics`, `mpv-adapter`, `cli`.
- ffmpeg-next build against system FFmpeg 4.4.2; libmpv dynamic link.
- `hw-diagnostics` reports decoder/encoder/interop/copy-back/rejection reason.
- CLI: probe, build index, decode single frame, export smoke, diagnose-hw, validate-edl, save-edl, load-edl, disk-space.
Acceptance:
- `cargo test --workspace` passes (headless).
- CLI probes a file and decodes a frame.
- Diagnostics reports hwdec status accurately on test hardware.
- clippy/fmt clean.
**Status: COMPLETE** (all crates implemented with tests)

## P2 — App shell + playback via libmpv + i18n/RTL foundation
Work:
- Tauri app shell, `tauri-plugin-player`, player controls (play/pause/seek/speed/volume/fullscreen/frame step).
- X11 child window creation via X11 FFI; Wayland detection; mpv `wid` embedding on X11, `set_wayland_mode()` on Wayland.
- i18n: en + ar, runtime switchable, RTL layout foundation, Fluent strings (or custom i18n in frontend).
- Dark/light/system theme; reduced-motion.
- MPRIS D-Bus integration (org.mpris.MediaPlayer2) via zbus: Root, Player, TrackList interfaces.
Acceptance:
- App launches and plays a file on X11 and Wayland.
- Language switch en<->ar at runtime flips direction and strings.
- Keyboard shortcuts (space, arrows, J/K/L, I/O, S, Delete) work.
- MPRIS controls work from system media indicator.
**Status: COMPLETE** (implemented in `apps/tauri-app`)

## P3 — Precision scrubbing
Work:
- Keyframe index (progressive), seek-and-decode, latest-wins cancellation, GOP/frame LRU.
- Timeline scrub bar: low-res preview while dragging, exact full-res on release.
- Frame step forward/backward.
- p50/p95 latency measurement on corpus.
- Tauri commands: `player_scrub_seek`, `player_scrub_next_frame`, `player_scrub_prev_frame`, `scrub_latency`.
Acceptance:
- Scrub p95 < 100 ms on 1080p (target; to validate).
- Frame-accurate preview on all-intra and long-GOP files.
- No dropped input during drag (latest-wins).
**Status: COMPLETE** (implemented in `scrub-engine` + Tauri commands)

## P4 — Lossless multi-segment cut + EDL + export engine (copy mode)
Work:
- `edit-engine` planning, `export-engine` stream-copy executor with timestamp rebasing.
- EDL JSON schema, save/load projects, undo/redo command pattern (in edit-engine).
- Cut modes: keep/remove segments, keyframe-aligned cut points shown to user.
- `project_delete_ranges` command for remove-ranges workflow.
- Atomic output (temp + rename), disk-space precheck, progress, cancellation.
Acceptance:
- Lossless export of `all-intra.mp4` segments is bit-identical to source segments (ffprobe compare).
- Non-keyframe cut in copy mode rejected with clear message + actual cut points shown.
- Project save/load round-trip; undo/redo works.
- Output written atomically; cancellation stops without corrupt file.
**Status: COMPLETE** (implemented in `edit-engine`, `export-engine`, Tauri commands)

## P5 — Ordered merge + compatibility analyzer + re-encode path + project save/undo
Work:
- Merge list: drag to reorder, per-clip trim, compatibility badge ("lossless OK" / "re-encode needed").
- Compatibility analyzer with reasons + alternative.
- Re-encode executor: libav encode, VAAPI/NVENC detect, sw fallback x264/x265/SVT-AV1, normalization (scale/pad/fps/audio resample).
- Quality presets (high/medium/low → preset + CRF mapping).
- `project_export` with `encoder_pref` and `quality_preset` parameters.
Acceptance:
- Merge matrix (spec 09.3) all expected outcomes.
- Re-encode output plays correctly; A/V sync within one frame at joins.
- HW encode used when VAAPI/NVENC available; sw fallback otherwise.
- Compatibility badge matches analyzer output.
**Status: COMPLETE** (implemented in `edit-engine`, `export-engine`, Tauri commands)

## P6 — Polish + web component + packaging + docs
Work:
- Subtitles (libass Arabic shaping + RTL), shortcuts, a11y (ARIA, focus order, contrast), MPRIS.
- Web component `<media-player>` playback only, tiered strategy, npm publish.
- .deb for Pop!_OS/Ubuntu 22.04+24.04; AppImage; Flatpak evaluation.
- GitHub Actions build + smoke test; README; docs.
Acceptance:
- a11y: keyboard nav, ARIA labels both languages, WCAG 2.2 AA contrast.
- Web component renders in WebKitGTK tier 1/2/3; tier 4 message shows for unsupported.
- .deb installs and runs on Pop!_OS 22.04 and 24.04.
- CI green: build, test, smoke, .deb.
**Status: IN PROGRESS** (web component implemented, packaging/docs pending)

## Long-term (post-v1, out of scope)
- Smart cut (boundary-GOP re-encode).
- Multi-track timeline, effects, color grading.
- Windows/macOS.
- DRM, streaming, library management.