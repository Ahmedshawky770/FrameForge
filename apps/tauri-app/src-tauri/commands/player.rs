use crate::setup;
use crate::state::{WaylandState, WindowXidState};
use frameforge_mpv_adapter::MpvPlayer;
use frameforge_tauri_plugin_player::PlayerState;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{command, State, WebviewWindow};
use tokio::time::sleep;

#[cfg(target_os = "linux")]
use std::os::raw::c_ulong;

#[command]
pub async fn player_load(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    xid_state: State<'_, Mutex<WindowXidState>>,
    wayland_state: State<'_, Mutex<WaylandState>>,
    src: String,
) -> Result<PlayerState, String> {
    let xid = xid_state.lock().map_err(|e| e.to_string())?.get_xid();
    let child_xid = xid_state.lock().map_err(|e| e.to_string())?.get_child_xid();
    let is_wayland = wayland_state
        .lock()
        .map_err(|e| e.to_string())?
        .is_wayland();
    {
        let mut p = handle.lock().map_err(|e| e.to_string())?;
        // Prefer the dedicated child window so mpv video stays inside #video-surface.
        let wid = child_xid.or(xid);
        frameforge_tauri_plugin_player::player_load_inner(&mut p, &src, wid, is_wayland)
            .map_err(|e| e.to_string())?;
        p.set_fullscreen(false);
    }

    // Small delay to let mpv start loading
    sleep(Duration::from_millis(500)).await;

    let p = handle.lock().map_err(|e| e.to_string())?;
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_set_fullscreen(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    fullscreen: bool,
) -> Result<PlayerState, String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    p.set_fullscreen(fullscreen);
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_play_pause(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
) -> Result<PlayerState, String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_play_pause_inner(&mut p);
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_seek(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    seconds: f64,
) -> Result<PlayerState, String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_seek_inner(&mut p, seconds);
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_set_speed(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    rate: f64,
) -> Result<PlayerState, String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_set_speed_inner(&mut p, rate);
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_set_volume(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    volume: f64,
) -> Result<PlayerState, String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_set_volume_inner(&mut p, volume);
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_set_muted(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    muted: bool,
) -> Result<PlayerState, String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_set_muted_inner(&mut p, muted);
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_step_frame(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
) -> Result<PlayerState, String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_step_frame_inner(&mut p);
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_toggle_fullscreen(window: State<'_, WebviewWindow>) -> Result<bool, String> {
    let current = window.is_fullscreen().unwrap_or(false);
    let next = !current;
    window.set_fullscreen(next).map_err(|e| e.to_string())?;
    Ok(next)
}

#[command]
pub async fn player_state(handle: State<'_, Arc<Mutex<MpvPlayer>>>) -> Result<PlayerState, String> {
    let p = handle.lock().map_err(|e| e.to_string())?;
    Ok(PlayerState::from_mpv(&p))
}

#[command]
pub async fn player_get_last_error(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
) -> Result<String, String> {
    let p = handle.lock().map_err(|e| e.to_string())?;
    Ok(frameforge_tauri_plugin_player::player_last_error(&p))
}

#[command]
pub async fn player_is_wayland(
    wayland_state: State<'_, Mutex<WaylandState>>,
) -> Result<bool, String> {
    Ok(wayland_state
        .lock()
        .map_err(|e| e.to_string())?
        .is_wayland())
}

#[command]
pub async fn player_set_hwdec(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    mode: String,
) -> Result<(), String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_set_hwdec(&mut p, &mode);
    Ok(())
}

#[command]
pub async fn player_get_hwdec(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
) -> Result<Option<String>, String> {
    let p = handle.lock().map_err(|e| e.to_string())?;
    Ok(frameforge_tauri_plugin_player::player_get_hwdec(&p))
}

#[command]
pub async fn player_set_vo(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    vo: String,
) -> Result<(), String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_set_vo(&mut p, &vo);
    Ok(())
}

#[command]
pub async fn player_get_vo(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
) -> Result<Option<String>, String> {
    let p = handle.lock().map_err(|e| e.to_string())?;
    Ok(frameforge_tauri_plugin_player::player_get_vo(&p))
}

#[command]
pub async fn player_get_volume(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
) -> Result<Option<f64>, String> {
    let p = handle.lock().map_err(|e| e.to_string())?;
    Ok(frameforge_tauri_plugin_player::player_get_volume(&p))
}

#[command]
pub async fn player_get_speed(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
) -> Result<Option<f64>, String> {
    let p = handle.lock().map_err(|e| e.to_string())?;
    Ok(frameforge_tauri_plugin_player::player_get_speed(&p))
}

/// Resize/move the mpv child X11 window to match #video-surface bounds.
/// Called from the frontend after load and on window resize.
#[command]
pub async fn player_resize_child_window(
    xid_state: State<'_, Mutex<WindowXidState>>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let child_xid = xid_state.lock().map_err(|e| e.to_string())?.get_child_xid();
    let Some(child) = child_xid else {
        return Ok(());
    };
    #[cfg(target_os = "linux")]
    {
        let display = unsafe { setup::shared_display() };
        if let Some(display) = display {
            unsafe {
                setup::resize_child_window(display, child as c_ulong, x, y, width, height);
            }
        } else {
            return Err("X11 display not available".to_string());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (child, x, y, width, height);
    Ok(())
}

#[command]
pub async fn player_set_child_window_visible(
    xid_state: State<'_, Mutex<WindowXidState>>,
    visible: bool,
) -> Result<(), String> {
    let child_xid = xid_state.lock().map_err(|e| e.to_string())?.get_child_xid();
    let Some(child) = child_xid else {
        return Ok(());
    };
    #[cfg(target_os = "linux")]
    {
        let display = unsafe { setup::shared_display() };
        if let Some(display) = display {
            unsafe {
                setup::set_child_window_visible(display, child as c_ulong, visible);
            }
        } else {
            return Err("X11 display not available".to_string());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (child, visible);
    Ok(())
}

use std::sync::Arc;

#[command]
pub async fn player_play_window(
    handle: State<'_, Arc<Mutex<MpvPlayer>>>,
    start: f64,
    end: f64,
) -> Result<u64, String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    p.play_window(start, end).map_err(|e| e.to_string())
}

#[command]
pub async fn player_clear_window(handle: State<'_, Arc<Mutex<MpvPlayer>>>) -> Result<(), String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    p.clear_window();
    Ok(())
}
