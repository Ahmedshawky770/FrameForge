#![allow(deprecated)]
use crate::mpris::MprisServer;
use crate::state::{WaylandState, WindowXidState};
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{LogicalSize, Manager, WebviewWindow};
use tokio::sync::Mutex as TokioMutex;

#[cfg(target_os = "linux")]
use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};

/// Detect if running on Wayland session
pub fn detect_wayland() -> bool {
    // Check WAYLAND_DISPLAY environment variable
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        return true;
    }
    // Could also check XDG_SESSION_TYPE
    if let Ok(session_type) = std::env::var("XDG_SESSION_TYPE") {
        if session_type == "wayland" {
            return true;
        }
    }
    false
}

/// Extract X11 window ID (XID) for mpv video embedding
#[cfg(target_os = "linux")]
pub fn extract_xid(window: &WebviewWindow) -> Option<u64> {
    if let Ok(raw_handle) = window.raw_window_handle() {
        let xid = match raw_handle {
            RawWindowHandle::Xlib(xlib_handle) => {
                // Xlib handle contains the window ID directly
                xlib_handle.window
            }
            RawWindowHandle::Xcb(xcb_handle) => {
                // XCB handle contains the window ID directly
                xcb_handle.window.get() as u64
            }
            RawWindowHandle::Wayland(_) => {
                println!(
                    "Wayland handle detected but WAYLAND_DISPLAY not set - treating as Wayland"
                );
                0
            }
            _ => {
                println!(
                    "Unsupported raw window handle type for XID extraction: {:?}",
                    raw_handle
                );
                0
            }
        };

        if xid != 0 {
            return Some(xid);
        }
    }
    None
}

#[cfg(target_os = "linux")]
#[link(name = "X11")]
extern "C" {
    fn XCreateSimpleWindow(
        display: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> c_ulong;
    fn XMapWindow(display: *mut c_void, w: c_ulong) -> c_int;
    fn XFlush(display: *mut c_void) -> c_int;
    fn XOpenDisplay(display_name: *const c_char) -> *mut c_void;
    fn XRaiseWindow(display: *mut c_void, w: c_ulong) -> c_int;
    fn XMoveResizeWindow(
        display: *mut c_void,
        w: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
    ) -> c_int;
}

/// Cached X11 display connection — opened once and reused across all X11 calls.
static X_DISPLAY: OnceLock<usize> = OnceLock::new();

/// Get (or lazily open) the shared X11 display connection.
#[cfg(target_os = "linux")]
/// # Safety
/// Calls X11 FFI; caller must ensure display pointer is valid.
pub unsafe fn shared_display() -> Option<*mut c_void> {
    let ptr = *X_DISPLAY.get_or_init(|| XOpenDisplay(std::ptr::null()) as usize);
    if ptr == 0 {
        None
    } else {
        Some(ptr as *mut c_void)
    }
}

/// Create a borderless X11 child window inside `parent` and map it.
#[cfg(target_os = "linux")]
/// # Safety
/// Calls X11 FFI; caller must ensure display/parent are valid.
pub unsafe fn create_child_window(
    display: *mut c_void,
    parent: c_ulong,
    x: c_int,
    y: c_int,
    w: c_uint,
    h: c_uint,
) -> Option<c_ulong> {
    let win = XCreateSimpleWindow(display, parent, x, y, w, h, 0, 0, 0);
    if win == 0 {
        return None;
    }
    XMapWindow(display, win);
    XFlush(display);
    // Raise above the WebView so video renders on top of the app UI
    // within the #video-surface region.
    XRaiseWindow(display, win);
    XFlush(display);
    println!(
        "[mpv-child] created child window id={} size={}x{} mapped=yes",
        win, w, h
    );
    Some(win)
}

/// Move/resize an existing mpv child X11 window.
/// XMapWindow and XRaiseWindow are done once at creation (create_child_window).
#[cfg(target_os = "linux")]
/// # Safety
/// Calls X11 FFI; caller must ensure display/win are valid.
pub unsafe fn resize_child_window(
    display: *mut c_void,
    win: c_ulong,
    x: c_int,
    y: c_int,
    w: c_uint,
    h: c_uint,
) {
    XMoveResizeWindow(display, win, x, y, w, h);
    XFlush(display);
    println!(
        "[mpv-child] resized child window id={} size={}x{} pos={}+{}",
        win, w, h, x, y
    );
}

/// Setup function called during Tauri app initialization
pub async fn setup(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let window = app.get_webview_window("main").ok_or_else(|| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "main window not found",
        ))
    })?;
    app.manage(window.clone());

    // Detect Wayland session
    let is_wayland = detect_wayland();
    if is_wayland {
        println!("Wayland session detected - using native mpv Wayland window");
    } else {
        println!("X11/XWayland session detected - using XID embedding");
    }

    let wayland_state = app.state::<Mutex<WaylandState>>();
    {
        let mut state = wayland_state
            .lock()
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())))?;
        state.set_wayland(is_wayland);
    }

    // Extract X11 window ID (XID) for mpv video embedding (only on X11)

    {
        if !is_wayland {
            if let Some(xid) = extract_xid(&window) {
                let xid_state = app.state::<Mutex<WindowXidState>>();
                let mut guard = xid_state.lock().unwrap_or_else(|e| e.into_inner());
                guard.set_xid(xid);
                println!("Set mpv wid (XID): {}", xid);

                // Create a dedicated borderless X11 child window inside the WebView window.
                // mpv renders into this child window so video is confined to #video-surface
                // instead of covering the whole application. Initial size is a placeholder;
                // the frontend resizes it to #video-surface after load and on window resize.
                let display = unsafe { shared_display() };
                if let Some(display) = display {
                    if let Some(child) =
                        unsafe { create_child_window(display, xid as c_ulong, 0, 0, 1280, 720) }
                    {
                        guard.set_child_xid(child);
                        println!("Created mpv child window (XID): {}", child);
                    }
                }
            }
        }
    }

    // Ensure window is not fullscreen and has reasonable constraints
    let _ = window.set_fullscreen(false);
    let _ = window.set_min_size(Some(LogicalSize {
        width: 800.0,
        height: 600.0,
    }));
    let _ = window.set_max_size(Some(LogicalSize {
        width: 3840.0,
        height: 2160.0,
    }));

    // Start MPRIS D-Bus service
    let mpris_server: Arc<TokioMutex<MprisServer>> =
        app.state::<Arc<TokioMutex<MprisServer>>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let mut mpris_guard = mpris_server.lock().await;
        let start_result = mpris_guard.start().await;
        if let Err(e) = start_result {
            eprintln!("Failed to start MPRIS: {}", e);
        }
    });

    Ok(())
}
