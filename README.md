# FrameForge

Linux-first video player/editing core and desktop app. Built in Rust (Tauri 2 + libmpv + FFmpeg libav* bindings) for Pop!_OS/Ubuntu, with a playback-only `<media-player>` web component.

## For whom
- You want a video cutter/merger on Linux (Pop!_OS primary) and couldn't find one you like.
- You want a reusable Rust core (headless-usable crates) to embed in your own desktop app.
- You want a playback-only web component for your site (no cut/merge in browser).

## Scope (v1)
**In:** playback, precision scrubbing, multi-segment cut (keep/remove), ordered merge, lossless stream-copy or re-encode export, EDL JSON projects, keyboard shortcuts, Arabic/RTL UI, `<media-player>` web component (playback only), MPRIS D-Bus integration, hardware decode/encode (VAAPI/NVDEC/NVENC), subtitle support via libass.
**Out:** multi-track timeline, effects/transitions/color grading, DRM, streaming service, library management, Windows/macOS in v1, microsecond precision, zero-copy across WebView boundary.

## Honest limitations
- Lossless cuts are keyframe-only; frame-accurate cuts require re-encode. Actual cut points are shown to the user.
- Web component is playback only; a normal web page cannot call native Rust/libmpv.
- Lossless merge requires all inputs to share codec/profile/res/fps/timebase/pixfmt/audio.
- Behavior is not guaranteed identical across desktop environments; see platform matrix.
- VAAPI/NVENC availability depends on host drivers; software fallback provided.

## Platform matrix (v1)
| Session | Display | GPU | Decode | Encode | Status |
|---------|---------|-----|--------|--------|--------|
| GNOME | X11 | Intel/AMD | VAAPI | VAAPI | Experimental |
| GNOME | Wayland | Intel/AMD | VAAPI | VAAPI | Experimental |
| GNOME | X11 | NVIDIA | NVDEC | NVENC | Experimental |
| GNOME | Wayland | NVIDIA | NVDEC | NVENC | Experimental |
| COSMIC | X11/Wayland | any | VAAPI/NVDEC | VAAPI/NVENC | Experimental |

Status key: Tested = verified on CI; Experimental = not yet validated; Not Supported = out of scope.

## Architecture
```
[ Rust core crates ] (headless, GUI-free)
  core-types | media-probe | scrub-engine | edit-engine | export-engine |
  mpv-adapter | hw-diagnostics | tauri-plugin-player | cli
       |
[ Tauri 2 app ] WebKitGTK UI (vanilla JS + Vite), en + ar RTL
       |
[ <media-player> web component ] playback only, tiered strategy
```

Memory-copy rule: every copy crossing the WebView process boundary is labeled "low-copy"; zero-copy is not claimed there.

## Quick start

### Prerequisites (Pop!_OS / Ubuntu 22.04+)
```bash
sudo apt update
sudo apt install -y libmpv-dev libavcodec-dev libavformat-dev libswscale-dev \
  libswresample-dev libavfilter-dev libavutil-dev libwebkit2gtk-4.0-dev \
  libgtk-3-dev libssl-dev pkg-config build-essential
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo install cargo-tauri
```

### Build & run the desktop app
```bash
git clone <repo> FrameForge && cd FrameForge
cargo tauri build   # or cargo tauri dev for dev mode
```

### Headless CLI
```bash
cargo run --bin frameforge-cli probe input.mp4
cargo run --bin frameforge-cli export --input input.mp4 --output out.mp4 --segments "0,5 10,15"
cargo run --bin frameforge-cli diagnose-hw
```

### Website — `<media-player>`
```bash
npm install @frameforge/media-player   # placeholder, TBD
```
```html
<media-player src="clip.mp4" locale="ar"></media-player>
<script>
  const p = document.querySelector('media-player');
  await p.play();
</script>
```

### Third-party desktop app (Rust)
```rust
use frameforge_core_types::{Edl, ExportSettings, ExportMode};
use frameforge_edit_engine::{plan_export, analyze_compatibility};
use frameforge_export_engine::{StreamCopyExecutor, ExportExecutor};
use frameforge_mpv_adapter::MpvPlayer;

let mut mpv = MpvPlayer::new()?;
mpv.load(std::path::Path::new("video.mp4"))?;
mpv.play();

// plan an export
let edl: Edl = serde_json::from_str(&project_json)?;
let plan = plan_export(&edl)?;
if !plan.merge_plan.lossless {
    println!("re-encode needed: {:?}", plan.merge_plan.reasons);
}

let mut exec: Box<dyn ExportExecutor> = if plan.merge_plan.lossless {
    Box::new(StreamCopyExecutor::new())
} else {
    Box::new(ReencodeExecutor::new())
};
exec.run(&plan, |p| println!("{:.1}%", p), cancel_token)?;
```

## Supported formats (container/codec, via libav)
MKV, MP4, MPEG-TS, WebM, MOV, AVI. Codecs: H.264, H.265/HEVC, VP8, VP9, AV1, MPEG-2, MPEG-4 Part 2; audio AAC, MP3, Opus, Vorbis, FLAC, PCM. Subtitles via libass. Actual support depends on FFmpeg build (system FFmpeg 4.4.2 on Ubuntu 22.04).

## License
Dual considerations: libmpv (LGPL), FFmpeg libav* (LGPL/GPL depending on config), x264/x265 (GPL). Dynamic linking preferred. Codec patents (H.264/H.265) are your responsibility in your jurisdiction. See `LICENSE` and ADR-1/ADR-4/ADR-5.

## Roadmap
P0 spike → P1 core → P2 app shell → P3 scrubbing → P4 cut/EDL/export (copy) → P5 merge/re-encode → P6 polish/web component/packaging.

## Benchmarks
Placeholder — no numbers invented. Benchmark harness in `bench/` measures cold start, idle RAM, playback RAM, scrub p50/p95, CPU with/without hwdec, export throughput. Results to be published after Phase 0 measurement. See `specs/09-testing-strategy.md`.

## Contributing
See `CONTRIBUTING.md`. File issues for bugs; check the risk register (`specs/12-risk-register.md`) before proposing major changes.

**العربية:** واجهة المستخدم تدعم العربية والإنجليزية مع التبديل في وقت التشغيل وتخطيط RTL. راجع قسم i18n في المواصفات (`specs/07-public-api.md`, `specs/08-key-algorithms.md`).