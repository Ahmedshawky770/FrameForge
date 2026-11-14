# FrameForge — Developer Quick Reference

This document is a condensed reference for developers working on FrameForge. See `specs/` for full specifications.

## Quick Commands

### Build & Run
```bash
# Desktop app (dev mode with hot reload)
cargo tauri dev

# Desktop app (release build)
cargo tauri build

# Run headless CLI
cargo run --bin frameforge-cli -- probe input.mp4
cargo run --bin frameforge-cli -- export --input in.mp4 --out out.mp4 --segments "0,5 10,15"
```

### Test
```bash
# All workspace tests (headless)
cargo test --workspace

# Specific crate
cargo test -p frameforge-scrub-engine
cargo test -p frameforge-edit-engine

# With ignored tests (requires test-media files)
cargo test --workspace -- --ignored
```

### Lint & Format
```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Architecture Overview

```
crates/ (headless, no GUI)
  core-types      → Timebase, Timestamp, EDL, Tracks, Errors
  media-probe     → ffmpeg probe + keyframe index
  scrub-engine    → latest-wins seek-and-decode
  edit-engine     → cut/merge planning, compatibility
  export-engine   → stream-copy + re-encode pipelines
  mpv-adapter     → libmpv wrapper (playback, tracks, subs)
  hw-diagnostics  → VAAPI/NVENC detection
  tauri-plugin-player → inner functions for Tauri commands

apps/tauri-app/ (Tauri 2 GUI)
  src-tauri/commands/  → Tauri #[command] handlers
  src-tauri/setup.rs   → Wayland detect, X11 child window
  src-tauri/mpris.rs   → MPRIS D-Bus (zbus)
  src/                 → vanilla JS + Vite frontend

packages/media-player/ (npm web component)
  src/index.ts      → <media-player> custom element
  src/tiers.ts      → Tier 1-4 playback implementations
  src/capability-detect.ts → browser capability detection
```

## Key Types (core-types)

```rust
// Time
Timebase { num: i64, den: i64 }          // e.g., 1/1_000_000 (microseconds)
Timestamp { value: i64, tb: Timebase }   // rational timestamp
TimeRange { start: Timestamp, end: Timestamp }

// Project
Edl { clips: Vec<Clip>, tracks: Vec<Track>, export: ExportSettings }
Clip { id: SourceId, path: PathBuf, segments: Vec<Segment> }
Segment { range: TimeRange, source_id: SourceId }
Track { kind: TrackKind, items: Vec<TimelineItem>, ... }
TimelineItem { source_range, timeline_start, track_index, ... }

// Export
ExportMode = StreamCopy | Reencode
Container = Mp4 | Mkv | Ts
```

## Tauri Commands (exact)

| Command | Params | Returns |
|---------|--------|---------|
| `locale_get` | — | `LocaleInfo { current, available, direction }` |
| `locale_set` | `locale: String` | `LocaleInfo` |
| `theme_get` | — | `ThemeInfo { current, effective }` |
| `theme_set` | `theme: String` | `ThemeInfo` |
| `player_load` | `src: String` | `PlayerState` |
| `player_play_pause` | — | `PlayerState` |
| `player_seek` | `seconds: f64` | `PlayerState` |
| `player_set_speed` | `rate: f64` | `PlayerState` |
| `player_set_volume` | `volume: f64` | `PlayerState` |
| `player_set_muted` | `muted: bool` | `PlayerState` |
| `player_step_frame` | — | `PlayerState` |
| `player_toggle_fullscreen` | — | `bool` |
| `player_state` | — | `PlayerState` |
| `player_get_last_error` | — | `String` |
| `player_set_hwdec` | `mode: String` | `()` |
| `player_get_hwdec` | — | `Option<String>` |
| `player_set_vo` | `vo: String` | `()` |
| `player_get_vo` | — | `Option<String>` |
| `player_get_volume` | — | `Option<f64>` |
| `player_get_speed` | — | `Option<f64>` |
| `player_is_wayland` | — | `bool` |
| `player_resize_child_window` | `x, y, width, height` | `()` |
| `player_scrub_seek` | `path, seconds` | `ScrubFrameResult { data_url, width, height, pts_seconds, latency_ms }` |
| `player_scrub_next_frame` | — | `ScrubFrameResult` |
| `player_scrub_prev_frame` | — | `ScrubFrameResult` |
| `scrub_latency` | — | `LatencyInfo { p50_ms, p95_ms, count }` |
| `player_get_tracks` | — | `Vec<TrackInfoResponse>` |
| `player_select_subtitle_track` | `index: u32` | `()` |
| `player_disable_subtitles` | — | `()` |
| `player_set_subtitle_visibility` | `visible: bool` | `()` |
| `player_set_subtitle_delay` | `seconds: f64` | `()` |
| `player_load_subtitle` | `path: String` | `()` |
| `open_subtitle_dialog` | — | `Option<String>` |
| `project_compatibility` | `clip_paths: Vec<String>` | `CompatibilityInfo` |
| `project_save` | `edl_json, path` | `()` |
| `project_load` | `path` | `String` (EDL JSON) |
| `project_export` | `edl_json, output_path, encoder_pref, quality_preset` | `()` |
| `project_export_cancel` | — | `()` |
| `project_export_progress` | — | `f32` (0.0-1.0) |
| `save_file_dialog` | — | `Option<String>` |
| `save_project_dialog` | — | `Option<String>` |
| `load_project_dialog` | — | `Option<String>` |
| `open_file_dialog` | — | `Option<String>` |
| `open_video_files_dialog` | — | `Option<Vec<String>>` |
| `project_delete_ranges` | `source_path, duration, delete_ranges` | `String` (EDL JSON) |
| `media_probe_duration` | `path` | `Option<f64>` |

## Frontend → Backend Data Flow

1. User drags timeline → frontend sends `player_scrub_seek(path, seconds)`
2. Backend `ScrubSession::seek_to_file()` → returns RGBA frame as base64 data URL
3. Frontend renders to `<canvas>` for preview
4. On export: frontend sends EDL JSON (f64 seconds) → `project_export`
5. Backend `ImportEdl::into_edl()` converts to microsecond `Timestamp`
6. `plan_export()` → `ExportPlan` with keyframe-snapped cut points
7. `StreamCopyExecutor` or `ReencodeExecutor` runs on blocking thread pool
8. Progress polled via `project_export_progress`; cancel via `project_export_cancel`

## Common Patterns

### Adding a Tauri command
1. Add function in `apps/tauri-app/src-tauri/commands/<module>.rs`
2. Export in `apps/tauri-app/src-tauri/commands/mod.rs`
3. Register in `main.rs` invoke_handler
4. Call from frontend: `await invoke('command_name', { param: value })`

### Adding a core type
1. Define in `crates/core-types/src/lib.rs`
2. Add `serde` derive for JSON serialization
3. Use in downstream crates via `frameforge_core_types::TypeName`

### Timebase conversion
```rust
// Frontend: seconds (f64)
// Backend: microseconds (Timebase { 1, 1_000_000 })
const IMPORT_TIMEBASE: Timebase = Timebase::new(1, 1_000_000);
let ts = Timestamp::from_seconds(frontend_seconds, IMPORT_TIMEBASE);
// Reverse:
let seconds = ts.to_seconds();
```

## Debugging Tips

| Issue | Check |
|-------|-------|
| Video not showing | X11: `player_is_wayland` returns false? XID extracted? Child window created? |
| Wayland black screen | `set_wayland_mode()` called? `gpu-next` + `wayland` context available? |
| Scrub slow | `scrub_latency` p95 > 100ms? HW decode enabled? Keyframe index built? |
| Export fails | `project_export_progress` stuck? Check `export_state.last_error` via `player_get_last_error` |
| Arabic RTL broken | `locale_get` returns `direction: "rtl"`? CSS `dir="rtl"` on html? |

## File Locations

| Feature | Key Files |
|---------|-----------|
| Video playback | `crates/mpv-adapter/src/lib.rs`, `apps/tauri-app/src-tauri/setup.rs` |
| Scrubbing | `crates/scrub-engine/src/lib.rs`, `apps/tauri-app/src-tauri/commands/scrub.rs` |
| Cut/merge | `crates/edit-engine/src/lib.rs`, `apps/tauri-app/src-tauri/commands/project.rs` |
| Export | `crates/export-engine/src/lib.rs` |
| i18n/RTL | `apps/tauri-app/src-tauri/commands/locale_theme.rs`, frontend CSS |
| MPRIS | `apps/tauri-app/src-tauri/mpris.rs` |
| Web component | `packages/media-player/src/` |

## Environment Variables

| Var | Purpose |
|-----|---------|
| `WAYLAND_DISPLAY` | Detect Wayland session |
| `XDG_SESSION_TYPE` | Detect Wayland (`wayland`) vs X11 (`x11`) |
| `RUST_LOG` | Debug logging (e.g., `RUST_LOG=debug cargo tauri dev`) |