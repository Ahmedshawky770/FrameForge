# Requirements Feasibility Table

Legend: Feasible = achievable with existing libs on target distro; Partial = works but with caveats/trade-offs; Not Feasible = requires scope change or new tech. All claims are assumptions until Phase 0 measurements confirm.

| # | Requirement | Status | Alternative / caveat |
|---|-------------|--------|----------------------|
| 1 | Play/pause/seek/speed/volume via libmpv on Linux | Feasible | libmpv render API or separate window; sync via libmpv to PipeWire/Pulse |
| 2 | Audio tracks / subtitles (Arabic shaping + RTL via libass) | Feasible | libmpv exposes tracks; libass handles shaping/RTL; font config needed for Arabic glyphs |
| 3 | Fullscreen, keyboard shortcuts, frame step | Feasible | Tauri global shortcuts + WebView focus handling; frame step via libmpv step frame |
| 4 | Frame-accurate preview while dragging (precision scrubbing) | Partial | Keyframe index + decode-forward; p95 latency bounded by frame duration + decode time; verify on WebKitGTK input path |
| 5 | Multi-segment cut (keep/remove), non-overlapping | Feasible | EDL-driven export plan; lossless only on keyframes |
| 6 | Ordered merge with reordering + per-clip trim | Feasible | Stream-copy remux or re-encode pipeline |
| 7 | Lossless stream-copy export when compatible | Feasible (conditional) | All inputs must share codec/profile/res/fps/timebase/pixfmt/audio; cuts keyframe-only; show actual cut points |
| 8 | Re-encode with HW (VAAPI/NVENC) + sw fallback | Partial | VAAPI/NVENC availability not guaranteed on Pop!_OS 22.04; verify driver install; sw x264/x265/SVT-AV1 fallback |
| 9 | Compatibility analyzer explaining non-lossless merges | Feasible | Compare stream parameters; produce human-readable reasons + alternative |
| 10 | Smart cut (re-encode boundary GOPs only) | Not Feasible v1 | Documented as later phase; complex GOP-boundary alignment |
| 11 | Serializable EDL JSON + save/load + undo/redo | Feasible | Serde JSON; command pattern for undo/redo in edit-engine |
| 12 | `<media-player>` web component playback only | Partial | Tiered strategy; WebKitGTK SharedArrayBuffer/COOP-COEP unverified; normal web page cannot call native libmpv |
| 13 | Native `<video>` -> MSE remux -> WebCodecs+WASM demuxer tiers | Partial | hls.js/mpegts.js/mux.js/mediabunny/web-demuxer evaluation needed; MKV support uncertain |
| 14 | English + Arabic runtime-switchable RTL | Feasible | Fluent or ICU4X; UAX #9 isolates; Arabic-Indic numeral option; mirroring table |
| 15 | No zero-copy across WebView boundary; label every copy | Feasible | Custom URI protocol + binary header; raw vs JPEG/WebP benchmark |
| 16 | No ffmpeg CLI; native ffmpeg-next + libmpv bindings | Feasible | ffmpeg-next vs FFmpeg 4.4.2 system headers needs build verification |
| 17 | Dynamic linking libmpv/FFmpeg; LGPL vs GPL docs | Feasible | x264/x265 GPL impact; codec patents; document build implications |
| 18 | Atomic output, disk-space precheck, progress, cancellation | Feasible | Temp file + rename; `df`/`statvfs` precheck; cancellation via mpv stop / abort encode |
| 19 | Works on X11 and Wayland (GNOME + COSMIC), NVIDIA | Partial | Wayland + WebView transparency/input-passthrough unverified; NVIDIA + GBM/EGL behavior unverified |
| 20 | Hardware decode retained (frames on GPU) | Partial | libmpv hwdec + VA-API; GPU->CPU download only for scrub previews; verify interop |
| 21 | .deb for Pop!_OS/Ubuntu 22.04+24.04, AppImage, Flatpak eval | Partial | AppImage WebKitGTK bundling pitfalls; Flatpak is evaluation only |
| 22 | CI build + smoke test (GitHub Actions) | Feasible | Linux runners; need headless Xvfb/Wayland for tests |
| 23 | MPRIS D-Bus integration | Feasible | zbus crate; org.mpris.MediaPlayer2 interface implemented |

## Key feasibility risks (require Phase 0 measurement)
- Video presentation strategy (A–E) on Wayland + WebView transparency.
- ffmpeg-next build against system FFmpeg 4.4.2.
- VAAPI/NVENC availability on Pop!_OS 22.04 out of the box.
- WebKitGTK SharedArrayBuffer / COOP-COEP for the web component data plane.