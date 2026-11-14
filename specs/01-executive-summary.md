# FrameForge — Executive Summary

## What
FrameForge is a reusable, Linux-first video player/editing core library plus a Tauri 2 desktop application. It supports playback, precision scrubbing, multi-segment cut, ordered merge, and export with a lossless-vs-re-encode decision engine. It ships a playback-only web component (`<media-player>`) for websites and a headless CLI.

## For whom
- Primary: the author (a full-stack web developer) building their own desktop video cutter/merger on Pop!_OS.
- Secondary: other Linux desktop developers who want a reusable Rust core (crates) they can embed in their own GTK/Tauri/Qt apps.
- Tertiary: website owners who want a playback-only `<media-player>` web component (no cut/merge in browser).

## Platform scope (v1)
Linux only. Primary targets: Pop!_OS 22.04 and current release, GNOME and COSMIC sessions, X11 and Wayland, NVIDIA/AMD/Intel. Windows/macOS are out of scope for v1 but must not be blocked by design choices.

## Core features (v1)
1. **Player**: play/pause/seek/speed/volume, audio tracks, subtitles (Arabic shaping + RTL via libass through libmpv), fullscreen, keyboard shortcuts, frame step, MPRIS D-Bus integration.
2. **Precision scrubbing**: frame-accurate preview while dragging the timeline via latest-wins seek-and-decode.
3. **Cut**: one or more non-overlapping segments; "keep these segments" or "remove these segments" modes via keyframe snapping.
4. **Merge**: ordered, reorderable clip list with optional per-clip trim; compatibility analyzer with human-readable reasons.
5. **Export**: lossless stream-copy (keyframe-aligned cuts only) or re-encode (libav, VAAPI/NVENC hw, x264/x265/SVT-AV1 sw fallback), quality presets, normalization, compatibility analyzer, atomic output (temp file + rename), disk-space precheck, progress events, cancellation.
6. **Project model**: serializable EDL as JSON with save/load and undo/redo (command pattern in edit-engine).
7. **Web component `<media-player>`**: playback only, tiered strategy (native `<video>` -> MSE remux -> WebCodecs + JS/WASM demuxer -> "unsupported" message).
8. **UI languages**: English and Arabic, runtime-switchable, RTL with mirroring table.
9. **Hardware diagnostics**: VAAPI/NVDEC/NVENC detection with rejection reasons.

## Non-goals
Multi-track timeline editor, effects/filters/transitions, color grading, DRM, streaming service, library management, Windows/macOS in v1, microsecond-precision claims, zero-copy across WebView boundary.

## Hard constraints recap
- Label every memory copy crossing the WebView process boundary ("low-copy"); no zero-copy claims there.
- No ffmpeg CLI; use ffmpeg-next/ffmpeg-sys-next and libmpv bindings.
- No SharedArrayBuffer unless COOP/COEP verified on WebKitGTK.
- Audio/A-V sync via libmpv delegated to PipeWire/PulseAudio.
- Prefer dynamic linking for libmpv/FFmpeg; document LGPL vs GPL and codec patent implications.
- Atomic output (temp file + rename), disk-space precheck, progress events, cancellation.