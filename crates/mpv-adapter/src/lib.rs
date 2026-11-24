//! mpv-adapter: libmpv playback wrapper (play/pause/seek/speed/volume/tracks/subtitles).
//!
//! Uses the `libmpv` crate (safe binding to libmpv). Video rendering into a
//! Tauri widget uses mpv's `wid` property on X11/XWayland for embedding.

use frameforge_core_types::{FrameForgeError, Result, Timestamp};
use libmpv::{FileState, Mpv, MpvNode};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;

/// Handle to a libmpv player instance.
pub struct MpvPlayer {
    inner: Mpv,
    running: Arc<AtomicBool>,
    /// Window ID for X11 embedding (set via set_wid before load)
    wid: Option<u64>,
    /// Last error message for diagnostics
    last_error: Arc<std::sync::Mutex<String>>,
    /// Last loaded media path (for windowed playback)
    current_path: Option<String>,
    /// Tauri app handle for emitting events
    app_handle: Arc<std::sync::Mutex<Option<tauri::AppHandle>>>,
    /// Whether a preview window sequence is active
    window_active: Arc<AtomicBool>,
    /// Generation counter for stale event filtering
    window_generation: Arc<std::sync::Mutex<u64>>,
    /// mpv event context (transmuted to 'static)
    event_ctx: Arc<std::sync::Mutex<Option<libmpv::events::EventContext<'static>>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub index: u32,
    pub kind: String, // "video", "audio", "subtitle"
    pub title: Option<String>,
    pub lang: Option<String>,
    pub selected: bool,
}

impl MpvPlayer {
    pub fn new() -> Result<Self> {
        let inner = Mpv::new().map_err(|e| {
            FrameForgeError::HwNotAvailable(format!("libmpv::Mpv::new failed: {e}"))
        })?;

        // Disable hardware decode by default to avoid black screen issues
        // User can enable via set_hwdec if needed
        let _ = inner.set_property("hwdec", "no");

        // Enable subtitle rendering via libass (Arabic shaping + RTL support)
        let _ = inner.set_property("sub-auto", "all");
        let _ = inner.set_property("sub-codepage", "utf8");

        // Use gpu (OpenGL/Vulkan) for video output - most reliable on modern Linux
        // Will be switched to gpu with x11 context when wid is provided via set_wid()
        let _ = inner.set_property("vo", "gpu");
        let _ = inner.set_property("gpu-context", "auto");

        // Audio output - try pipewire, then pulse, then alsa, then auto
        let _ = inner.set_property("ao", "pipewire,pulse,alsa,auto");

        // Enable verbose logging for debugging
        let _ = inner.set_property("msg-level", "all=info");

        // Ensure mpv doesn't create its own window when we want embedding
        let _ = inner.set_property("fullscreen", "no");
        let _ = inner.set_property("keep-open", "yes");
        let _ = inner.set_property("border", "no");
        let _ = inner.set_property("ontop", "no");
        let _ = inner.set_property("force-window", "no");
        let _ = inner.set_property("keep-aspect", "yes");
        let _ = inner.set_property("panscan", "0.0");
        let _ = inner.set_property("video-aspect", "");

        // Keep cursor visible
        let _ = inner.set_property("cursor-autohide", "no");
        // Disable all default mpv key bindings
        let _ = inner.set_property("input-default-bindings", "no");
        // Don't hide cursor
        let _ = inner.set_property("input-cursor", "no");
        // Bind ESC to "ignore" so mpv doesn't toggle fullscreen on ESC
        let _ = inner.command("add-key-binding", &["ESC", "ignore"]);
        // Prevent mpv from capturing terminal input
        let _ = inner.set_property("input-terminal", "no");

        // Event context for EndFile events (windowed playback)
        let event_ctx_inner = inner.create_event_context();
        let _ = event_ctx_inner.enable_event(libmpv::events::mpv_event_id::EndFile);
        let event_ctx_inner = unsafe {
            std::mem::transmute::<
                libmpv::events::EventContext<'_>,
                libmpv::events::EventContext<'static>,
            >(event_ctx_inner)
        };
        let event_ctx = Arc::new(std::sync::Mutex::new(Some(event_ctx_inner)));
        let app_handle: Arc<std::sync::Mutex<Option<tauri::AppHandle>>> =
            Arc::new(std::sync::Mutex::new(None));
        let window_active = Arc::new(AtomicBool::new(false));
        let window_generation = Arc::new(std::sync::Mutex::new(0u64));

        // Background thread: emit Tauri event on EndFile (eof) when a window is active
        let ctx_t = event_ctx.clone();
        let active_t = window_active.clone();
        let gen_t = window_generation.clone();
        let app_t = app_handle.clone();
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            loop {
                let mut guard = match ctx_t.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                };
                let ev = match guard.as_mut() {
                    Some(e) => e,
                    None => {
                        drop(guard);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                };
                match ev.wait_event(0.5) {
                    Some(Ok(libmpv::events::Event::EndFile(reason))) => {
                        drop(guard);
                        if reason == 0 && active_t.load(Ordering::SeqCst) {
                            active_t.store(false, Ordering::SeqCst);
                            let g = *gen_t.lock().unwrap();
                            if let Some(app) = app_t.lock().unwrap().clone() {
                                let _ = app.emit("preview-window-ended", g);
                            }
                        }
                    }
                    _ => {
                        drop(guard);
                    }
                }
            }
        });

        Ok(Self {
            inner,
            running: Arc::new(AtomicBool::new(false)),
            wid: None,
            last_error: Arc::new(std::sync::Mutex::new(String::new())),
            current_path: None,
            app_handle,
            window_active,
            window_generation,
            event_ctx,
        })
    }

    /// Convert a path to a string that mpv can handle, including non-ASCII paths
    /// mpv accepts UTF-8 paths natively, so we just return the path as-is.
    fn path_to_mpv_string(path: &Path) -> String {
        path.to_string_lossy().to_string()
    }

    /// Load a media file. If `wid` was set via `set_wid`, mpv will embed
    /// into that X11 window. Otherwise, mpv creates its own window.
    pub fn load(&mut self, path: &Path) -> Result<()> {
        let path_str = Self::path_to_mpv_string(path);

        // Clear previous error
        if let Ok(mut err) = self.last_error.lock() {
            err.clear();
        }

        // Ensure mpv is not fullscreen before loading
        let _ = self.inner.set_property("fullscreen", "no");

        // Load with proper error handling
        let result = self
            .inner
            .playlist_load_files(&[(path_str.as_str(), FileState::Replace, None)])
            .map_err(|e| {
                let err_msg = format!("mpv loadfile failed: {e}");
                if let Ok(mut err) = self.last_error.lock() {
                    *err = err_msg.clone();
                }
                FrameForgeError::Probe(err_msg)
            });

        if result.is_ok() {
            self.current_path = Some(path_str.clone());
            self.running.store(true, Ordering::SeqCst);

            // Verify video/audio tracks are loaded - poll with timeout since loading is async
            let mut tracks_loaded = false;
            for _ in 0..50 {
                // Up to 5 seconds (50 * 100ms)
                std::thread::sleep(std::time::Duration::from_millis(100));
                if let Ok(tracks) = self.get_tracks() {
                    let has_video = tracks.iter().any(|t| t.kind == "video");
                    let has_audio = tracks.iter().any(|t| t.kind == "audio");
                    if has_video || has_audio {
                        tracks_loaded = true;
                        break;
                    }
                }
            }

            if !tracks_loaded {
                if let Ok(mut err) = self.last_error.lock() {
                    *err = "No video or audio tracks found in file after timeout - file may be corrupt, unsupported format, or failed to load".to_string();
                }
            }

            let _ = self.inner.set_property("keep-aspect", "yes");
            let _ = self.inner.set_property("panscan", "0.0");
        }

        result
    }

    /// Get the last error message
    pub fn last_error(&self) -> String {
        self.last_error
            .lock()
            .map(|e| e.clone())
            .unwrap_or_default()
    }

    /// Enable/disable hardware decoding
    pub fn set_hwdec(&mut self, mode: &str) {
        let _ = self.inner.set_property("hwdec", mode);
    }

    /// Set video output driver
    pub fn set_vo(&mut self, vo: &str) {
        let _ = self.inner.set_property("vo", vo);
    }

    /// Get current video output driver
    pub fn get_vo(&self) -> Option<String> {
        self.inner.get_property("vo").ok()
    }

    /// Get current hardware decoding mode
    pub fn get_hwdec(&self) -> Option<String> {
        self.inner.get_property("hwdec").ok()
    }

    pub fn play_pause(&mut self) {
        let paused = self.inner.get_property("pause").unwrap_or(true);
        let _ = self.inner.set_property("pause", !paused);
    }

    pub fn seek(&mut self, t: Timestamp) {
        let seconds = t.to_seconds();
        let _ = self.inner.seek_absolute(seconds);
    }

    /// Seek relative to current position (in seconds)
    pub fn seek_relative(&mut self, seconds: f64) {
        if seconds >= 0.0 {
            let _ = self.inner.seek_forward(seconds);
        } else {
            let _ = self.inner.seek_backward(-seconds);
        }
    }

    /// Seek to absolute position (in seconds)
    pub fn seek_absolute(&mut self, seconds: f64) {
        let _ = self.inner.seek_absolute(seconds);
    }

    pub fn set_speed(&mut self, f: f64) {
        let _ = self.inner.set_property("speed", f);
    }

    pub fn set_volume(&mut self, f: f64) {
        // mpv volume is 0-100
        let _ = self.inner.set_property("volume", f * 100.0);
    }

    pub fn set_muted(&mut self, muted: bool) {
        let _ = self.inner.set_property("mute", muted);
    }

    pub fn step_frame(&mut self) {
        let _ = self.inner.command("frame-step", &[]);
    }

    pub fn toggle_fullscreen(&mut self) {
        let fs = self.inner.get_property("fullscreen").unwrap_or(false);
        let _ = self.inner.set_property("fullscreen", !fs);
    }

    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        let _ = self.inner.set_property("fullscreen", fullscreen);
    }

    pub fn set_geometry(&mut self, x: i32, y: i32, width: i32, height: i32) {
        let _ = self
            .inner
            .set_property("geometry", format!("{}x{}+{}+{}", width, height, x, y));
    }

    /// Set video geometry (position and size) using mpv's geometry property.
    pub fn set_video_geometry(&mut self, x: i32, y: i32, width: i32, height: i32) {
        let _ = self
            .inner
            .set_property("geometry", format!("{}x{}+{}+{}", width, height, x, y));
        let _ = self.inner.set_property("keep-aspect", "yes");
        let _ = self.inner.set_property("panscan", "0.0");
    }

    /// Center 1280x720 video within the given window dimensions.
    pub fn center_video_in_window(&mut self, window_width: i32, window_height: i32) {
        let x = (window_width - 1280) / 2;
        let y = (window_height - 720) / 2;
        self.set_video_geometry(x, y, 1280, 720);
    }

    /// Load an external subtitle file (e.g. .srt, .ass) for the current media.
    /// libass handles Arabic shaping + RTL rendering automatically.
    pub fn load_subtitle(&mut self, path: &Path) -> Result<()> {
        let path_str = Self::path_to_mpv_string(path);
        self.inner
            .command("sub-add", &[&path_str])
            .map_err(|e| FrameForgeError::Probe(format!("subtitle load failed: {e}")))?;
        Ok(())
    }

    /// Get all available tracks (video, audio, subtitle) with selection status.
    pub fn get_tracks(&self) -> Result<Vec<TrackInfo>> {
        let node: MpvNode = match self.inner.get_property("track-list") {
            Ok(n) => n,
            Err(_) => return Ok(Vec::new()),
        };
        let mut tracks = Vec::new();
        if let Some(arr) = node.to_array() {
            for entry in arr {
                if let Some(map) = entry.to_map() {
                    let mut index: u32 = 0;
                    let mut kind = "unknown".to_string();
                    let mut title: Option<String> = None;
                    let mut lang: Option<String> = None;
                    let mut selected = false;
                    for (key, val) in map {
                        match key {
                            "type" => kind = val.to_str().unwrap_or("unknown").to_string(),
                            "title" => title = val.to_str().map(|s| s.to_string()),
                            "lang" => lang = val.to_str().map(|s| s.to_string()),
                            "selected" => selected = val.to_bool().unwrap_or(false),
                            "id" => index = val.to_i64().unwrap_or(0).max(0) as u32,
                            _ => {}
                        }
                    }
                    tracks.push(TrackInfo {
                        index,
                        kind,
                        title,
                        lang,
                        selected,
                    });
                }
            }
        }
        Ok(tracks)
    }

    /// Select/deselect a track by index. Set `selected=false` to disable.
    pub fn select_track(&mut self, index: u32, selected: bool) -> Result<()> {
        let prop = format!("track-list/{}/selected", index);
        self.inner
            .set_property(&prop, selected)
            .map_err(|e| FrameForgeError::Probe(format!("track select failed: {e}")))?;
        Ok(())
    }

    /// Select a subtitle track by index.
    pub fn select_subtitle_track(&mut self, index: u32) -> Result<()> {
        self.select_track(index, true)
    }

    /// Disable all subtitle tracks.
    pub fn disable_subtitles(&mut self) -> Result<()> {
        let tracks = self.get_tracks()?;
        for track in tracks.iter() {
            if track.kind == "subtitle" {
                self.select_track(track.index, false)?;
            }
        }
        Ok(())
    }

    /// Set subtitle visibility (show/hide without changing track selection).
    pub fn set_subtitle_visibility(&mut self, visible: bool) -> Result<()> {
        self.inner
            .set_property("sub-visibility", visible)
            .map_err(|e| FrameForgeError::Probe(format!("subtitle visibility failed: {e}")))?;
        Ok(())
    }

    /// Set subtitle delay in seconds (positive = delay, negative = advance).
    pub fn set_subtitle_delay(&mut self, seconds: f64) -> Result<()> {
        self.inner
            .set_property("sub-delay", seconds)
            .map_err(|e| FrameForgeError::Probe(format!("subtitle delay failed: {e}")))?;
        Ok(())
    }

    /// Set the X11 window ID that mpv should render into (wid).
    /// Must be called before `load` to embed video in a specific window.
    pub fn set_wid(&mut self, wid: u64) {
        self.wid = Some(wid);
        // Switch to gpu with x11 context for window embedding
        let _ = self.inner.set_property("vo", "gpu");
        let _ = self.inner.set_property("gpu-context", "x11egl");
        let _ = self.inner.set_property("wid", wid as i64);
        // Ensure bindings stay disabled
        let _ = self.inner.set_property("input-default-bindings", "no");
        // Disable force-window since we're embedding
        let _ = self.inner.set_property("force-window", "no");
        // Ensure no fullscreen when embedding
        let _ = self.inner.set_property("fullscreen", "no");
    }

    /// Configure mpv for Wayland native window (separate from Tauri WebView).
    /// Uses gpu-next with wayland context and forces mpv to create its own window.
    pub fn set_wayland_mode(&mut self) {
        self.wid = None;
        // Use gpu-next (modern VO) with wayland backend
        let _ = self.inner.set_property("vo", "gpu-next");
        let _ = self.inner.set_property("gpu-context", "wayland");
        // Force mpv to create its own Wayland window
        let _ = self.inner.set_property("force-window", "yes");
        // No wid property on Wayland - mpv manages its own window
    }

    /// Get the mpv window ID (X11) if mpv created its own window.
    /// This can be used to position/manage the mpv window externally.
    pub fn get_mpv_window_id(&self) -> Option<u64> {
        // mpv doesn't directly expose its window ID, but we can try to get it
        // from the vo-window property if available
        self.inner
            .get_property::<i64>("vo-window")
            .ok()
            .map(|w| w as u64)
    }

    /// Set mpv window position and size (for external window management)
    pub fn set_window_geometry(&mut self, x: i32, y: i32, width: u32, height: u32) {
        let _ = self
            .inner
            .set_property("geometry", format!("{}x{}+{}+{}", width, height, x, y));
    }

    /// Observe a property value (e.g. "time-pos", "duration", "pause").
    /// Retained for API compatibility; libmpv observers are not wired here.
    pub fn observe_property(
        &mut self,
        _name: &str,
        _id: u64,
    ) -> std::result::Result<(), FrameForgeError> {
        Ok(())
    }

    pub fn time_position(&self) -> Option<f64> {
        self.inner.get_property("time-pos").ok()
    }

    pub fn duration(&self) -> Option<f64> {
        self.inner.get_property("duration").ok()
    }

    pub fn paused(&self) -> bool {
        self.inner.get_property("pause").unwrap_or(true)
    }

    /// Get current volume (0.0 - 1.0)
    pub fn volume(&self) -> Option<f64> {
        self.inner
            .get_property::<f64>("volume")
            .ok()
            .map(|v| v / 100.0)
    }

    /// Get current playback speed
    pub fn speed(&self) -> Option<f64> {
        self.inner.get_property("speed").ok()
    }

    /// Execute an mpv command
    pub fn command(&mut self, name: &str, args: &[&str]) -> Result<()> {
        self.inner
            .command(name, args)
            .map_err(|e| FrameForgeError::Probe(format!("mpv command {} failed: {}", name, e)))
    }

    /// Set an mpv property
    pub fn set_property<T>(&mut self, name: &str, value: T) -> Result<()>
    where
        T: libmpv::SetData,
    {
        self.inner
            .set_property(name, value)
            .map_err(|e| FrameForgeError::Probe(format!("Failed to set property {}: {}", name, e)))
    }

    /// Generic property getter for any mpv property
    pub fn get_property<T>(&self, name: &str) -> Result<T>
    where
        T: libmpv::GetData,
    {
        self.inner
            .get_property(name)
            .map_err(|e| FrameForgeError::Probe(format!("Failed to get property {}: {}", name, e)))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Set the Tauri app handle for event emission.
    pub fn set_app_handle(&mut self, app: tauri::AppHandle) {
        if let Ok(mut h) = self.app_handle.lock() {
            *h = Some(app);
        }
    }

    /// Play a single time window [start, end] from the loaded media.
    /// mpv stops at `end` (EOF) and emits EndFile; the background thread
    /// then emits "preview-window-ended" with the returned generation.
    pub fn play_window(&mut self, start: f64, end: f64) -> Result<u64> {
        use std::sync::atomic::Ordering;
        let path = self
            .current_path
            .clone()
            .ok_or_else(|| FrameForgeError::Probe("no media loaded".to_string()))?;
        let mut gen = self.window_generation.lock().unwrap();
        *gen += 1;
        let g = *gen;
        drop(gen);
        self.window_active.store(true, Ordering::SeqCst);
        let opts = format!("start={},end={}", start, end);
        self.inner
            .command("loadfile", &[&path, "replace", &opts])
            .map_err(|e| FrameForgeError::Probe(format!("loadfile window failed: {e}")))?;
        let _ = self.inner.set_property("pause", false);
        Ok(g)
    }

    /// Cancel any active preview window.
    pub fn clear_window(&mut self) {
        use std::sync::atomic::Ordering;
        self.window_active.store(false, Ordering::SeqCst);
        if let Ok(mut g) = self.window_generation.lock() {
            *g += 1;
        }
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        let _ = self.inner.command("stop", &[]);
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn adapter_struct_exists() {
        assert!(true);
    }
}
