# Risk Register

Top 10 technical/legal/maintenance risks with mitigations. Status: identified (not yet mitigated until phases execute).

| ID | Risk | Impact | Likelihood | Mitigation |
|----|------|--------|------------|------------|
| R1 | Strategy A (libmpv `wid` into X11 child window) fails on Wayland — no `wid` equivalent. | High (blocks P2 playback on Wayland) | High | Phase 0 measures A–E; fallback Strategy B on Wayland (native mpv window via gpu-next/wayland, implemented in `set_wayland_mode()`); do not commit to UI stack until P0 passes. |
| R2 | ffmpeg-next incompatible with system FFmpeg 4.4.2 (Ubuntu 22.04) — build or runtime breakage. | High (blocks P1 core) | Medium | Verify build against system headers early; pin ffmpeg-next version; vendor newer FFmpeg or build from source if needed; document. |
| R3 | VAAPI/NVENC not available out of the box on Pop!_OS 22.04 — HW decode/encode claims unmet. | Medium | High | Software fallback (x264/x265/SVT-AV1); `hw-diagnostics` reports rejection reason; document driver install requirements; do not promise HW. |
| R4 | WebKitGTK SharedArrayBuffer / COOP-COEP not usable — web component data plane limited. | Medium (web tier only) | Medium | Avoid SharedArrayBuffer unless verified; use Tauri commands with base64 for scrub previews; tier 4 fallback message. |
| R5 | NVIDIA + Wayland + GBM/EGL interop unstable — black frames, no hwdec. | Medium | Medium | Test on NVIDIA hardware; prefer X11 for NVIDIA in v1; document known issues; software fallback. |
| R6 | libmpv / FFmpeg LGPL vs x264/x265 GPL + codec patents (H.264/H.265). | Legal/Medium | Medium | Dynamic linking; document license implications; avoid shipping GPL encoders by default or provide opt-in; patent compliance is user's responsibility. |
| R7 | Arabic/RTL rendering bugs (shaping, bidi, numerals, mirroring) ship to users. | Medium | Medium | Fluent i18n (or custom), UAX #9 isolates, mirroring table, RTL screenshot tests, pseudo-localization, Arabic font stack. |
| R8 | A/V sync drift at segment joins (merge/export), especially open-GOP/B-frames/VFR. | High | Medium | Clamp DTS per stream, warn on drift, re-encode fallback for non-keyframe cuts, test matrix, golden-frame sync tests. |
| R9 | App crashes/corrupt output on cancellation or low disk. | Medium | Medium | Atomic output (temp + rename), disk-space precheck, cancellation tokens, latest-wins abort, tests for cancel-corruption. |
| R10 | Core crates accumulate UI/Tauri dependence, breaking headless reuse. | Medium | Low | Enforce dependency direction in CI (`ui depends on core, never reverse`); headless CLI smoke test each CI run. |
| R11 | X11 child window resize race — video surface misaligned with #video-surface. | Medium | Medium | Frontend calls `player_resize_child_window` after load and on window resize; mpv `panscan=1.0` fills child window. |
| R12 | MPRIS D-Bus registration fails on headless CI (no session bus). | Low | High | MPRIS start is non-blocking; gracefully handle connection failure; skip on CI or use dbus-daemon. |

## Assumptions in this register
- Pop!_OS 22.04 baseline; newer FFmpeg/libmpv may arrive via backports/PPA. (Assumption)
- NVIDIA hardware not present in this shell; NVIDIA risks unverified. (Assumption)
- Wayland testing requires nested compositor (weston/Xvfb) not available in current environment. (Assumption)