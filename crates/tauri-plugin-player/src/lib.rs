//! tauri-plugin-player: Tauri player library (no #[command] macros here — those
//! live in main.rs to avoid Tauri 2 duplicate-macro errors).

use std::path::Path;

use frameforge_core_types::{FrameForgeError, Timebase, Timestamp};
use frameforge_mpv_adapter::{MpvPlayer, TrackInfo as MpvTrackInfo};

/// Serializable player state sent to the WebView on every change.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlayerState {
    pub paused: bool,
    pub current_time: f64,
    pub duration: f64,
    pub volume: f64,
    pub playback_rate: f64,
}

/// Serializable track info for the UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrackInfo {
    pub index: u32,
    pub kind: String,
    pub title: Option<String>,
    pub lang: Option<String>,
    pub selected: bool,
}

impl From<MpvTrackInfo> for TrackInfo {
    fn from(t: MpvTrackInfo) -> Self {
        Self {
            index: t.index,
            kind: t.kind,
            title: t.title,
            lang: t.lang,
            selected: t.selected,
        }
    }
}

/// Tauri-managed player state. MpvPlayer is not Sync, so we wrap in Mutex.
pub type PlayerHandle = std::sync::Mutex<MpvPlayer>;

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            paused: true,
            current_time: 0.0,
            duration: 0.0,
            volume: 1.0,
            playback_rate: 1.0,
        }
    }
}

impl PlayerState {
    pub fn from_mpv(p: &MpvPlayer) -> Self {
        Self {
            paused: p.paused(),
            current_time: p.time_position().unwrap_or(0.0),
            duration: p.duration().unwrap_or(0.0),
            volume: p.volume().unwrap_or(1.0),
            playback_rate: p.speed().unwrap_or(1.0),
        }
    }
}

/// Load a media file into the player.
/// If `wid` is provided, sets the mpv `wid` property before loading
/// (required for X11/XWayland video embedding into the Tauri window).
/// If `is_wayland` is true, configures mpv for Wayland native window
/// (gpu-next with wayland context, force-window=yes).
pub fn player_load_inner(
    p: &mut MpvPlayer,
    src: &str,
    wid: Option<u64>,
    is_wayland: bool,
) -> std::result::Result<(), FrameForgeError> {
    if is_wayland {
        p.set_wayland_mode();
    } else if let Some(w) = wid {
        p.set_wid(w);
        // Force mpv to render into the provided child window (not the whole WebView).
        // These must be set AFTER set_wid() because set_wid() resets force-window to "no".
        let _ = p.set_property("force-window", "yes");
        let _ = p.set_property("border", "no");
        let _ = p.set_property("keep-aspect", "yes");
        let _ = p.set_property("input-default-bindings", "no");
        let _ = p.set_property("fullscreen", "no");
    }
    // Load first so mpv has a video track, then force panscan to fill the child window.
    p.load(Path::new(src))?;
    if !is_wayland {
        // Fill the child window completely (crop to fit, preserve aspect).
        let _ = p.set_property("panscan", 1.0);
    }
    Ok(())
}

/// Toggle play/pause.
pub fn player_play_pause_inner(p: &mut MpvPlayer) {
    p.play_pause();
}

/// Seek to a position (seconds).
pub fn player_seek_inner(p: &mut MpvPlayer, seconds: f64) {
    let tb = Timebase::new(1, 1_000_000);
    let ts = Timestamp::new((seconds * 1_000_000.0) as i64, tb);
    p.seek(ts);
}

/// Set playback speed.
pub fn player_set_speed_inner(p: &mut MpvPlayer, rate: f64) {
    p.set_speed(rate);
}

/// Set volume (0.0 - 1.0).
pub fn player_set_volume_inner(p: &mut MpvPlayer, volume: f64) {
    p.set_volume(volume);
}

/// Set muted.
pub fn player_set_muted_inner(p: &mut MpvPlayer, muted: bool) {
    p.set_muted(muted);
}

/// Step forward one frame.
pub fn player_step_frame_inner(p: &mut MpvPlayer) {
    p.step_frame();
}

/// Toggle fullscreen.
pub fn player_toggle_fullscreen_inner(p: &mut MpvPlayer) {
    p.toggle_fullscreen();
}

/// Load an external subtitle file (e.g. .srt, .ass).
/// libass handles Arabic shaping + RTL rendering automatically.
pub fn player_load_subtitle_inner(p: &mut MpvPlayer, path: &str) -> Result<(), FrameForgeError> {
    p.load_subtitle(Path::new(path))
}

/// Get all available tracks (video, audio, subtitle).
pub fn player_get_tracks_inner(p: &MpvPlayer) -> Result<Vec<TrackInfo>, FrameForgeError> {
    let mpv_tracks = p.get_tracks()?;
    Ok(mpv_tracks.into_iter().map(TrackInfo::from).collect())
}

/// Select/deselect a track by index.
pub fn player_select_track_inner(
    p: &mut MpvPlayer,
    index: u32,
    selected: bool,
) -> Result<(), FrameForgeError> {
    p.select_track(index, selected)
}

/// Select a subtitle track by index.
pub fn player_select_subtitle_track_inner(
    p: &mut MpvPlayer,
    index: u32,
) -> Result<(), FrameForgeError> {
    p.select_subtitle_track(index)
}

/// Disable all subtitle tracks.
pub fn player_disable_subtitles_inner(p: &mut MpvPlayer) -> Result<(), FrameForgeError> {
    p.disable_subtitles()
}

/// Set subtitle visibility (show/hide).
pub fn player_set_subtitle_visibility_inner(
    p: &mut MpvPlayer,
    visible: bool,
) -> Result<(), FrameForgeError> {
    p.set_subtitle_visibility(visible)
}

/// Set subtitle delay in seconds (positive = delay, negative = advance).
pub fn player_set_subtitle_delay_inner(
    p: &mut MpvPlayer,
    seconds: f64,
) -> Result<(), FrameForgeError> {
    p.set_subtitle_delay(seconds)
}

/// Get last error message from mpv
pub fn player_last_error(p: &MpvPlayer) -> String {
    p.last_error()
}

/// Set hardware decoding mode
pub fn player_set_hwdec(p: &mut MpvPlayer, mode: &str) {
    p.set_hwdec(mode);
}

/// Get current hardware decoding mode
pub fn player_get_hwdec(p: &MpvPlayer) -> Option<String> {
    p.get_hwdec()
}

/// Get current volume (0.0 - 1.0)
pub fn player_get_volume(p: &MpvPlayer) -> Option<f64> {
    p.volume()
}

/// Get current playback speed
pub fn player_get_speed(p: &MpvPlayer) -> Option<f64> {
    p.speed()
}

/// Set video output driver
pub fn player_set_vo(p: &mut MpvPlayer, vo: &str) {
    p.set_vo(vo);
}

/// Get current video output driver
pub fn player_get_vo(p: &MpvPlayer) -> Option<String> {
    p.get_vo()
}

/// Initialize the player plugin (no-op for now; setup happens in main.rs).
pub fn init() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_state_default() {
        let s = PlayerState::default();
        assert!(s.paused);
    }
}
