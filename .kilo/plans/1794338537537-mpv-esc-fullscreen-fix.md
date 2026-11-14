# mpv ESC/Fullscreen Fix Plan

## Problem
1. Video opens in fullscreen mode covering entire window
2. Pressing ESC triggers fullscreen toggle (like F11) instead of exiting fullscreen
3. mpv intercepts all keyboard input including ESC
4. mpv's gpu video output draws on entire X11 window surface, covering WebView UI

## Root Causes
1. mpv's `vo=gpu` creates OpenGL surface covering entire X11 window
2. mpv's default key bindings: ESC = cycle fullscreen
3. No input blocking - keyboard events go directly to mpv
4. No CSS z-layering to keep WebView UI above mpv video

## Files to Modify

### 1. `crates/mpv-adapter/src/lib.rs`
- Add `input-terminal = no` to prevent mpv from capturing terminal input
- Add `cursor-autohide = no` to keep cursor visible
- Bind ESC key to "ignore" or "no-op" using `input-bindings`
- Set mpv geometry to a reasonable default (e.g., 1280x720 centered)
- Add `keep-open = yes`, `idle = yes`

### 2. `apps/tauri-app/src-tauri/index.html`
- Add CSS to ensure WebView UI stays above mpv video
- Set `#video-surface` to have controlled size, not full window
- Add `z-index` hierarchy: UI controls > video surface

### 3. `apps/tauri-app/src-tauri/src/main.rs` (or the JS frontend)
- Intercept ESC key in JavaScript to prevent it reaching mpv
- Handle ESC to exit any fullscreen state properly
- Call `player_set_fullscreen(false)` on ESC

## Implementation Steps
1. Update mpv-adapter with input bindings and geometry
2. Update HTML/CSS for proper layering
3. Add ESC key handler in frontend JavaScript
4. Compile and verify

## Expected Result
- Video embeds in a defined portion of window, not fullscreen
- ESC key exits any fullscreen mode (doesn't trigger it)
- WebView buttons remain visible and clickable
- UI responds to all interactions