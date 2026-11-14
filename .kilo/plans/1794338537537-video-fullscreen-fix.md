# Video Fullscreen Fix Plan

## Problem
When opening a video in FrameForge desktop app, mpv takes over the entire X11 window surface via XID embedding, covering the WebView UI. Result: video appears fullscreen, no buttons visible, UI unresponsive.

## Root Cause
1. mpv's video output (`vo=gpu`) claims the entire X11 window surface when embedding via `wid`
2. mpv's `fullscreen` property may default to true or be set by mpv internally
3. No CSS layering to keep WebView UI above mpv video
4. No window geometry constraints on mpv output

## Files to Modify

### 1. `crates/mpv-adapter/src/lib.rs`
- Add `fullscreen = no` property in `new()` 
- Add `keep-open = yes` property
- Add window geometry management
- Ensure `force-window = no` is properly set before load

### 2. `apps/tauri-app/src-tauri/main.rs`
- Add Tauri command to set mpv `fullscreen` property
- Add command to set mpv window geometry
- Ensure `player_load` sets `fullscreen = no` before loading

### 3. `apps/tauri-app/src-tauri/setup.rs`
- Set window size constraints (min/max)
- Ensure window is not fullscreen by default

### 4. Frontend CSS (in Tauri app dist or inline)
- Add CSS to position UI elements above video
- Ensure WebView container doesn't cover entire screen

## Implementation Steps
1. Add `fullscreen = no` and `keep-open = yes` to mpv adapter
2. Add `geometry` property to control video position/size
3. Add Tauri command `player_set_fullscreen(false)` called during load
4. Add CSS to keep UI controls visible
5. Verify compilation and runtime behavior

## Expected Result
- Video embeds in a portion of the window, not fullscreen
- WebView buttons remain visible and clickable
- UI responds to all interactions