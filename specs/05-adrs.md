# ADRs (Architecture Decision Records)

## ADR-1: libmpv vs custom playback (libav + custom renderer)
**Decision:** Use libmpv (libmpv2 crate, dynamic linking) as the playback engine.
**Rationale:** libmpv already solves A/V sync, PipeWire/PulseAudio output, hwdec (VAAPI/NVDEC), subtitle rendering via libass (Arabic shaping + RTL), track selection, seek semantics, frame step, and MPRIS integration. Building a custom libav renderer would reimplement years of sync/edge-case work.
**Trade-offs:** LGPL (libmpv) + GPL implications if linked against x264/x265 via mpv; NVIDIA/Wayland interop quirks; mpv version pinning needed (0.34.x on Pop!_OS 22.04).
**Alternatives considered:** Custom libav renderer (rejected — too much sync work), GStreamer (rejected — larger dependency surface, Arabic/RTL subtitle handling less direct).
**Verification needed:** libmpv X11 child window embedding under WebKitGTK on X11; Wayland native window via gpu-next (Phase 0).

## ADR-2: Video presentation (Strategies A–E)
**Decision:** Strategy A on X11/XWayland (dedicated X11 child window created via X11 FFI, mpv `wid` embedding, verified). Strategy B on Wayland (native mpv window with gpu-next/wayland backend, `force-window=yes`, implemented in `set_wayland_mode()`). Strategy E retained for future evaluation if native GTK4/libadwaita shell proves viable.
**Rationale:** X11 child window embedding works reliably on X11/XWayland with lowest copy count and video confined to `#video-surface`. On Wayland, native mpv window avoids WebKitGTK transparency/input-passthrough complexity and uses gpu-next for zero-copy presentation. Strategy E is a longer-term architectural shift.
**Alternatives:** C (canvas streaming) rejected for full playback due to RGBA bandwidth (1080p60 ≈ 497 MB/s raw, 4K60 ≈ 1990 MB/s); viable only for scrub previews. D (proxies) rejected as primary — adds disk/time cost and doesn't solve source-accurate scrub.

## ADR-3: IPC transport (control + data plane)
**Decision:** Control plane = typed Tauri commands/events (serde JSON). Data plane (scrub previews) = base64-encoded RGBA frames returned via Tauri commands (not custom URI protocol). `ipc-bridge` crate removed as unused.
**Rationale:** Tauri commands are well-typed for both control and data; base64 overhead is acceptable for scrub previews (lower frequency than full playback) and avoids custom URI protocol complexity. No SharedArrayBuffer needed.
**Alternatives considered:** SharedArrayBuffer (rejected unless COOP/COEP verified), WebSocket over localhost (added complexity), custom URI protocol (added complexity, removed), Tauri `emit` with base64 (kept for control, used for data plane).

## ADR-4: Hardware acceleration strategy
**Decision:** Auto-detect VAAPI and NVDEC/CUVID for decode; VAAPI and NVENC for encode; software fallback (x264/x265/SVT-AV1). Keep frames on GPU during playback; GPU->CPU download only for scrub previews. `hw-diagnostics` crate reports active decoder/encoder, interop mode, driver, copy-back status, and rejection reason.
**Rationale:** VAAPI is the Linux-wide decode/encode path (Intel/AMD); NVDEC/NVENC for NVIDIA. Software fallback guarantees functionality on any hardware.
**Trade-offs:** NVENC is encoder-only (not a decoder on most consumer cards); VAAPI driver install not guaranteed on Pop!_OS 22.04; patent/license on x264/x265 (GPL).
**Verification needed:** `vainfo`, VAAPI driver presence, NVIDIA driver/NVENC on target hardware.

## ADR-5: Lossless vs re-encode decision policy
**Decision:** Lossless stream-copy only when ALL inputs share: codec, profile, resolution, frame rate, timebase, pixel format, audio codec/sample rate/channels, AND cuts land on keyframes. Otherwise re-encode with normalization (scale/pad, fps, audio resample). Show user actual cut points and a compatibility analyzer explaining why lossless is impossible.
**Rationale:** Stream-copy is bit-perfect and fast but cannot do frame-accurate cuts mid-GOP or normalize mismatched inputs.
**Alternatives:** Smart cut (re-encode boundary GOPs only) deferred to later phase.
**Documentation:** Every case where lossless is impossible is documented with the fallback chosen.

## ADR-6: i18n framework
**Decision:** Fluent (FluentBundle / FluentResource) or ICU4X for runtime-switched English + Arabic; externalized strings only; UAX #9 isolates for bidi; Arabic-Indic numeral option; 6 Arabic plural forms; mirroring table (transport icons and timecode digits/video do NOT mirror).
**Rationale:** Fluent is mature, supports complex plural/gender, and runtime locale switching. ICU4X is lighter but smaller ecosystem; choose based on bundle-size measurement.
**Alternatives considered:** gettext (rejected — weak plural/runtime switching), fluent-rs bindings.
**Verification needed:** Arabic font stack + line-height on Pop!_OS; RTL screenshot tests.

## ADR-7: Packaging
**Decision:** Primary .deb for Pop!_OS/Ubuntu 22.04 and 24.04 (exact deps: libwebkit2gtk, libmpv, libav*). AppImage as secondary (document WebKitGTK bundling pitfalls + GPU/VAAPI driver assumptions). Flatpak as evaluation only.
**Rationale:** .deb integrates with Pop!_OS apt and gives exact dependency control. AppImage gives portability but bundles WebKitGTK (large) and depends on host drivers. Flatpak sandboxing complicates VAAPI/NVENC device access.
**Alternatives considered:** Snap (rejected — not preferred on Pop!_OS ecosystem), source-only (rejected — poor UX).

## ADR-8: Frontend framework (vanilla JS + Vite)
**Decision:** Use vanilla JavaScript with Vite for the Tauri app frontend (no Svelte/Preact/React).
**Rationale:** Minimal dependencies, fast build, no framework overhead, easier debugging, full control over DOM for video surface integration. The UI is transport controls + timeline — simple enough for vanilla JS.
**Alternatives considered:** Svelte (adds compiler step), Preact/React (adds bundle size and complexity for simple UI).
**Verification needed:** RTL layout with vanilla JS, Arabic text rendering.

## ADR-9: Timebase (microsecond rational timestamps)
**Decision:** All internal timestamps use `Timebase(1, 1_000_000)` (microseconds) with `Timestamp { value: i64, tb: Timebase }`. No float PTS anywhere. Frontend sends/receives seconds (f64); backend converts to microsecond timebase.
**Rationale:** Avoids floating-point precision issues; rational timebase enables exact frame arithmetic; microsecond granularity sufficient for frame-accurate operations.
**Alternatives considered:** nanosecond (overkill, larger ints), float seconds (precision loss), frame index (breaks with VFR).