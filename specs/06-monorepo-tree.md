# Monorepo Tree

Cargo workspace (GUI-free core, headless-usable) + Tauri app + npm workspace for the web component.
UI depends on core; core never depends on UI.

```
FrameForge/
├── .github/workflows/              # CI: build, clippy, fmt, test, smoke test, .deb build
├── .kilo/                          # Kilo config (commands, agents, skills)
├── AGENTS.md                       # Agent instructions for this repo
├── kilo.json                       # Kilo project config
├── Cargo.toml                      # Workspace root manifest
├── Cargo.lock                      # Locked dependencies
├── rust-toolchain.toml             # Pin Rust toolchain
├── justfile                        # Task runner (build, test, bench, pkg)
├── README.md                       # Deliverable 11
├── LICENSE                         # GPL-3.0-or-later with LGPL exceptions
├── CONTRIBUTING.md                 # Contribution guidelines
├── specs/                          # The 12 deliverable specs (this folder)
│   ├── 01-executive-summary.md
│   ├── 02-feasibility-table.md
│   ├── 03-phase0-spike-adr.md
│   ├── 04-architecture-diagrams.md
│   ├── 05-adrs.md
│   ├── 06-monorepo-tree.md
│   ├── 07-public-api.md
│   ├── 08-key-algorithms.md
│   ├── 09-testing-strategy.md
│   ├── 10-roadmap.md
│   ├── 11-readme.md
│   └── 12-risk-register.md
│
├── crates/                         # Cargo workspace members (GUI-free core)
│   ├── core-types/                 # time/rational timebase, errors, EDL types, TimelineItem, Track, Transition
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # Time, Timebase, Timestamp, TimeRange, SourceId, ItemId, CutMode, Segment, Clip, TrackKind, Track, TimelineItem, Transition, ExportMode, Container, ExportSettings, Edl, FrameForgeError, Result, ffmpeg_init
│   ├── media-probe/                # probe + keyframe index
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # probe_stream, keyframe_index, index_file, ProbeResult, StreamInfo, FormatInfo, KeyframeIndex
│   ├── scrub-engine/               # seek-and-decode, latest-wins scrubbing
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # ScrubSession, seek_to, next_frame, prev_frame, DecodedFrame, CancelToken, ScrubResult
│   ├── edit-engine/                # cut/merge planning, compatibility analyzer, EDL validation, ripple edit
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # plan_export, compatibility_analyzer, validate_edl, ripple_edit, insert_item_ripple, remove_item_ripple, trim_item, ExportPlan, PlannedSegment, MergePlan, TrackPlan, CompatibilityReport, SnapMode
│   ├── export-engine/              # stream-copy remux + re-encode pipelines
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # StreamCopyExecutor, ReencodeExecutor, CancelToken, ExportExecutor, disk_space_free, atomic_write, output_format_from_extension
│   ├── mpv-adapter/                # libmpv bindings wrapper
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # MpvPlayer, hwdec, subtitles, tracks, Wayland mode, X11 wid, MPRIS-ready
│   ├── hw-diagnostics/             # VAAPI/NVENC detect + diagnostics report
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # probe_hw, Diagnostics (decoder, encoder, interop, copy_back, hw_rejected_reason)
│   └── tauri-plugin-player/        # Tauri plugin exposing player to WebView (inner functions, no #[command])
│       ├── Cargo.toml
│       └── src/lib.rs              # PlayerState, TrackInfo, player_load_inner, player_*_inner functions
│
├── apps/
│   └── tauri-app/                  # Tauri 2 desktop application (UI depends on core)
│       ├── Cargo.toml              # Tauri app manifest (depends on crates/*)
│       ├── src-tauri/
│       │   ├── Cargo.toml
│       │   ├── lib.rs              # module exports: state, commands, setup, mpris
│       │   ├── main.rs             # Tauri app entry, state management, command registration, MPRIS setup
│       │   ├── commands/
│       │   │   ├── mod.rs
│       │   │   ├── locale_theme.rs # locale_get/set, theme_get/set
│       │   │   ├── player.rs       # player_load, player_play_pause, player_seek, player_set_speed, player_set_volume, player_set_muted, player_step_frame, player_toggle_fullscreen, player_state, player_get_last_error, player_set_hwdec, player_get_hwdec, player_set_vo, player_get_vo, player_get_volume, player_get_speed, player_is_wayland, player_resize_child_window
│       │   │   ├── scrub.rs        # player_scrub_seek, player_scrub_next_frame, player_scrub_prev_frame, scrub_latency
│       │   │   ├── subtitle.rs     # player_get_tracks, player_select_subtitle_track, player_disable_subtitles, player_set_subtitle_visibility, player_set_subtitle_delay, player_load_subtitle, open_subtitle_dialog
│       │   │   └── project.rs      # project_compatibility, project_save, project_load, project_export, project_export_cancel, project_export_progress, save_file_dialog, save_project_dialog, load_project_dialog, open_file_dialog, open_video_files_dialog, project_delete_ranges, media_probe_duration
│       │   ├── setup.rs            # Wayland detection, X11 XID extraction, X11 child window creation (XCreateSimpleWindow)
│       │   ├── state.rs            # WindowXidState, WaylandState, LocaleState, ThemeState, ScrubState, LatencySamples, ExportState, TrackInfoResponse, CompatibilityInfo
│       │   ├── mpris.rs            # MPRIS D-Bus (org.mpris.MediaPlayer2) via zbus: Root, Player, TrackList interfaces
│       │   └── capabilities.json   # Tauri capabilities (auto-generated)
│       ├── src/                    # frontend (vanilla JS + Vite)
│       │   ├── main.js             # app entry, Tauri command imports, event listeners
│       │   ├── index.html          # UI: video surface, transport controls, timeline, track panel, export dialog
│       │   └── styles/
│       │       └── main.css        # styling with CSS custom properties, RTL support
│
├── packages/                       # npm workspace
│   └── media-player/               # playback-only web component `<media-player>`
│       ├── package.json
│       ├── rollup.config.js
│       ├── tsconfig.json
│       ├── src/
│       │   ├── index.ts            # MediaPlayerElement custom element registration
│       │   ├── index.d.ts          # TypeScript declarations
│       │   ├── tiers.ts            # Tier 1-4 implementations (MediaPlayerBase)
│       │   └── capability-detect.ts # detectCapabilities(), getSupportedExtensions()
│       ├── test/
│       └── README.md
│
├── cli/                            # headless tool (depends on crates/*)
│   ├── Cargo.toml
│   └── src/main.rs                 # probe, build-index, diagnose-hw, validate-edl, save-edl, load-edl, disk-space, export
│
├── bench/                          # benchmark harness
│   ├── Cargo.toml
│   └── src/main.rs                 # p50/p95 scrub latency, export throughput, RAM
│
├── test-media/                     # media corpus (not committed large files; LFS or script)
│   ├── corpus.yaml                 # list of test files + checksums
│   ├── test-video.mp4              # 1080p H.264 test file
│   ├── all-intra.mp4               # (expected) all-intra for keyframe testing
│   ├── long-gop.ts                 # (expected) MPEG-TS long GOP
│   ├── 4k-hevc.mkv                 # (expected) 4K HEVC
│   ├── vfr.mkv                     # (expected) VFR
│   ├── mixed-audio.mkv             # (expected) multiple audio tracks
│   └── arabic-sub.mkv              # (expected) Arabic subtitles
│
├── docs/                           # user-facing docs (ADR summaries, API guide)
└── packaging/
    ├── debian/                     # .deb packaging (control, rules, install)
    ├── appimage/                   # AppImage recipe
    └── flatpak/                    # Flatpak (evaluation only)
```

Folder conventions:
- `crates/*` = library crates (no GUI, headless-safe).
- `apps/tauri-app` = the Tauri GUI; only place with WebKitGTK/GTK dependence.
- `packages/media-player` = npm-published web component.
- `cli/` = headless Rust binary for tests and benchmarks.
- `bench/` = regression-harness benchmarks.
- `packaging/` = dist artifacts.