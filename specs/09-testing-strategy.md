# Testing Strategy

## 9.1 Media corpus
Test files (committed via Git LFS or fetched by checksum at test time; not large binaries in repo):
- `test-video.mp4` — 1080p H.264, general playback test (committed, 216 MB)
- `all-intra.mp4` — all-intra, every frame is a keyframe; tests lossless cut at any point.
- `long-gop.ts` — MPEG-TS, long GOP, no index; tests partial-index fallback.
- `4k-hevc.mkv` — 4K HEVC; tests HW decode path, RAM, 4K scrub.
- `vfr.mkv` — variable frame rate; tests PTS-not-index seeking, VFR->CFR normalization.
- `mixed-audio.mkv` — multiple audio tracks + languages; tests track selection.
- `arabic-sub.mkv` — libass Arabic subtitle with shaping + RTL; tests subtitle rendering.
- `different-fps.mp4` + `different-res.mp4` — merge compatibility matrix inputs.

Corpus manifest `test-media/corpus.yaml` with sha256 checksums; tests download/generate if missing.

## 9.2 Golden-frame tests
- Decode a known frame at a known PTS from `all-intra.mp4`; compare pixel hash to golden hash.
- Seek to same PTS via keyframe-index path and direct path; results must match.
- Golden hashes stored as text; regenerate only on intentional codec/mpv version change.
- Run on X11 and Wayland; golden may differ slightly by compositor/hwdec — note as known issue.
- Implemented in `crates/scrub-engine/tests/golden_frame.rs::golden_frame_test()`

## 9.3 Merge-compatibility matrix tests
| Input A | Input B | Expected lossless? | Expected fallback |
|-----------|-----------|--------------------|-------------------|
| same codec/res/fps/audio | same | Yes | stream-copy |
| same codec, different res | different | No | re-encode + scale/pad |
| same codec, different fps | different | No | re-encode + fps normalize |
| different audio layout (stereo vs mono) | different | No | re-encode + resample |
| CFR vs VFR | different | No | re-encode + CFR normalize |
| B-frame vs no B-frame | maybe | No (safe) | re-encode |
| open-GOP | closed-GOP | No (safe) | re-encode |
| MP4 vs MKV container (same streams) | maybe | Yes if streams compatible | remux container |

Test: `analyze_compatibility` returns correct report; export produces valid output playable by ffprobe/mpv; A/V sync within one frame at joins.

## 9.4 Scrub latency tests (p50/p95)
For each corpus file: 10 seeks, measure input->frame-displayed latency. Report p50/p95.
- 1080p30, 1080p60, 4K HEVC, VFR.
- With and without hwdec.
- Thresholds (to validate): p50 < 50 ms, p95 < 100 ms (1080p); 4K may exceed — document.
- Implemented in `crates/scrub-engine/tests/golden_frame.rs` and `scrub_latency` Tauri command.

## 9.5 RTL / i18n tests
- Fluent `.ftl` files load for `en` and `ar`; missing keys fall back to English, not empty.
- Runtime switch en<->ar without reload; `document.dir` flips.
- Arabic plural forms (6) render correctly for counts 0,1,2,3-10,100+.
- UAX #9 isolates around mixed Arabic/Latin filenames; no display corruption.
- Arabic-Indic vs Western numeral toggle.
- RTL screenshot tests: screenshot key screens in `ar` layout; compare to baseline (CI).
- Long-string overflow tests: longest Arabic string in each UI element; no clipping.
- Pseudo-localization: replace strings with accented/lengthened versions to catch hardcoded strings.

## 9.6 DE matrix (Pop!_OS GNOME/COSMIC x X11/Wayland x GPU vendor)
| Session | Display | GPU | hwdec | Video presents | Input/overlay | Status |
|---------|---------|-----|-------|----------------|---------------|--------|
| GNOME | X11 | Intel | VAAPI | ? | ? | Experimental |
| GNOME | Wayland | Intel | VAAPI | ? | ? | Experimental |
| GNOME | X11 | AMD | VAAPI | ? | ? | Experimental |
| GNOME | Wayland | AMD | VAAPI | ? | ? | Experimental |
| GNOME | X11 | NVIDIA | NVDEC | ? | ? | Experimental |
| GNOME | Wayland | NVIDIA | NVDEC | ? | ? | Experimental |
| COSMIC | X11/Wayland | any | any | ? | ? | Experimental |

"?" = untested until Phase 0 + CI matrix runs. Mark Tested/Experimental/Not Supported.

## 9.7 Accessibility checks
- Keyboard navigation: Tab order flips in RTL; all controls reachable via keyboard.
- ARIA labels in en and ar; verify with screen-reader audit (manual).
- Contrast target WCAG 2.2 AA (contrast checker on design tokens).
- Reduced-motion: respect `prefers-reduced-motion`; disable timeline animations.
- Focus visible indicator in both themes.

## 9.8 CI regression gate
- `cargo clippy --all-targets -- -D warnings`
- `cargo fmt --check`
- `cargo test --workspace` (headless crates)
- Golden-frame test pass/fail
- Scrub latency p95 < threshold (fail CI if regression > 20%)
- Merge matrix: all expected outcomes
- `.deb` build success (GitHub Actions Linux)
- Smoke test: app launches, plays `test-video.mp4` for 30s without crash (headless Xvfb or nested Wayland)

## 9.9 CLI tests
- `probe`, `build-index`, `diagnose-hw`, `validate-edl`, `save-edl`, `load-edl`, `disk-space`, `export` commands work end-to-end.
- EDL JSON round-trip: save → load → validate → export.

## Assumptions
- Golden-frame hashes are stable across mpv/libav patch versions on the same distro. (Assumption; may need per-distro baselines)
- CI runners can run WebKitGTK/Tauri headlessly (Xvfb or weston). (Assumption — verify)