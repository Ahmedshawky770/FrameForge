#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use frameforge_mpv_adapter::MpvPlayer;
use frameforge_tauri_app::commands::{locale_theme, player, project, scrub, subtitle};
use frameforge_tauri_app::mpris::MprisServer;
use frameforge_tauri_app::setup;
use frameforge_tauri_app::state::{
    ExportState, LocaleState, ScrubState, ThemeState, WaylandState, WindowXidState,
};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TokioMutex;

fn main() {
    let player = Arc::new(Mutex::new(
        MpvPlayer::new().expect("failed to create mpv player"),
    ));
    let mpris_player = player.clone();

    // Export progress / cancellation state shared between project_export and
    // the cancel/progress poll commands.
    let export_state = Arc::new(Mutex::new(ExportState::default()));

    // Create MPRIS server before Tauri builder so we can start it after setup
    let mpris_server = Arc::new(TokioMutex::new(
        MprisServer::new(mpris_player.clone()).unwrap(),
    ));

    tauri::Builder::default()
        .manage(player)
        .manage(Mutex::new(LocaleState::default()))
        .manage(Mutex::new(ThemeState::default()))
        .manage(Mutex::new(ScrubState::default()))
        .manage(Mutex::new(WindowXidState::default()))
        .manage(Mutex::new(WaylandState::default()))
        .manage(export_state)
        .manage(mpris_server)
        .invoke_handler(tauri::generate_handler![
            // i18n
            locale_theme::locale_get,
            locale_theme::locale_set,
            // theme
            locale_theme::theme_get,
            locale_theme::theme_set,
            // scrubbing
            scrub::player_scrub_seek,
            scrub::player_scrub_next_frame,
            scrub::player_scrub_prev_frame,
            scrub::scrub_latency,
            // player
            player::player_load,
            player::player_set_fullscreen,
            player::player_play_pause,
            player::player_seek,
            player::player_play_window,
            player::player_clear_window,
            player::player_set_speed,
            player::player_set_volume,
            player::player_set_muted,
            player::player_toggle_fullscreen,
            player::player_state,
            // diagnostics
            player::player_get_last_error,
            player::player_set_hwdec,
            player::player_get_hwdec,
            player::player_set_vo,
            player::player_get_vo,
            player::player_is_wayland,
            // mpv child window resize (X11)
            player::player_resize_child_window,
            // mpv child window visibility (X11) for WebView overlays
            player::player_set_child_window_visible,
            // subtitles
            subtitle::player_get_tracks,
            subtitle::player_select_subtitle_track,
            subtitle::player_disable_subtitles,
            subtitle::player_set_subtitle_visibility,
            subtitle::player_set_subtitle_delay,
            subtitle::player_load_subtitle,
            subtitle::open_subtitle_dialog,
            // project management
            project::project_compatibility,
            project::project_save,
            project::project_load,
            project::project_export,
            project::project_export_cancel,
            project::project_export_progress,
            project::project_export_encoder,
            project::save_file_dialog,
            project::save_project_dialog,
            project::load_project_dialog,
            project::open_file_dialog,
            project::open_video_files_dialog,
            project::project_delete_ranges,
            project::project_export_delete_ranges,
            project::media_probe_duration,
        ])
        .setup(|app| tauri::async_runtime::block_on(async move { setup::setup(app).await }))
        .run(tauri::generate_context!())
        .expect("error while running tauris application");
}
