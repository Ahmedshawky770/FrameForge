# Video Fullscreen Fix Plan

## Root Cause
When loading a video in FrameForge desktop app, mpv embeds via XID into the Tauri/WebKitGTK window. mpv's `vo=gpu` OpenGL surface renders the video covering the **entire X11 window**, making the video appear fullscreen and covering all UI buttons/inputs. The UI elements exist underneath the mpv video surface and mouse clicks on the video area go to mpv, not to the UI.

## Files Modified

### 1. `crates/mpv-adapter/src/lib.rs`
- Added `keep-aspect = "yes"`, `panscan = "0.0"`, `video-aspect = ""` in `MpvPlayer::new()` — video maintains aspect ratio, no cropping
- Added `input-default-bindings = "no"` — disables all default mpv keyboard shortcuts
- Added `input-terminal = "no"` — mpv won't capture terminal input
- Added `input-cursor = "no"` — cursor stays visible
- Bound ESC key to `ignore` via `add-key-binding` — ESC won't toggle fullscreen
- Added `cursor-autohide = "no"` — cursor stays visible
- Added `window-max-width = 1280`, `window-max-height = 720`, `autofit = "1280x720"` — restricts video to center area
- In `set_wid()`: added geometry `1280x720+0+0` and `input-default-bindings = "no"`
- In `load()`: added `keep-aspect = "yes"` and `panscan = "0.0"` after successful load
- Added `set_video_geometry()` method
- Added `center_video_in_window(window_width, window_height)` method — centers 1280x720 video

### 2. `apps/tauri-app/src-tauri/commands/player.rs`
- `player_load`: calls `p.set_fullscreen(false)` after load_inner
- Added `player_set_fullscreen` command
- Added `player_set_video_geometry` command
- Added `player_center_video_in_window` command

### 3. `apps/tauri-app/src-tauri/main.rs`
- Registered `player_set_fullscreen`, `player_set_video_geometry`, `player_center_video_in_window` in invoke_handler

### 4. `apps/tauri-app/src-tauri/setup.rs`
- Added `window.set_fullscreen(false)`, `set_min_size(800x600)`, `set_max_size(3840x2160)`
- Window constraints prevent excessive resizing

### 5. `apps/tauri-app/src-tauri/tauri.conf.json`
- Changed `"transparent": false` to `"transparent": true` — WebView transparency allows HTML/CSS UI to show through where mpv doesn't render video

### 6. `apps/tauri-app/index.html` (frontend)
- `#app`: added `position: relative;`
- `#video-surface`: changed from `flex: 1` to fixed positioned centered area `1280x720` with `max-height: calc(90vh - var(--timeline-height))`, `z-index: 10`
- UI elements: added `pointer-events: auto`, `position: relative`, `z-index: 100` for timeline/project-panel
- `#placeholder`: added `position: relative; z-index: 30`
- `#overlay`/`#overlay.visible`: proper z-index and pointer-events

### 7. `apps/tauri-app/index.html` (frontend JS)
- ESC key: calls `player_toggle_fullscreen` if in fullscreen, then explicitly calls `player_set_fullscreen({ fullscreen: false })`
- After `player_load`: 1 second setTimeout calls `player_set_fullscreen({ fullscreen: false })` as safety measure

## Verification
- `cargo check --workspace`: compiles cleanly (only pre-existing warnings)
- `cargo test --workspace`: all 47 tests pass, 0 failures

## Expected Result After Fixes
- Video embeds in a defined centered area (1280x720 max), not fullscreen
- WebView UI buttons remain visible and clickable above the video area
- ESC key exits any fullscreen mode (doesn't trigger it)
- WebKitGTK transparency allows HTML/CSS UI to show through where mpv doesn't render video
- MPV keyboard bindings disabled, all input handled by frontend
