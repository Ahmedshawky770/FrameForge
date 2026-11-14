use crate::state::TrackInfoResponse;
use frameforge_mpv_adapter::MpvPlayer;
use frameforge_tauri_plugin_player;
use std::sync::Mutex;
use tauri::{command, State};

#[command]
pub async fn player_get_tracks(
    handle: State<'_, Mutex<MpvPlayer>>,
) -> Result<Vec<TrackInfoResponse>, String> {
    let p = handle.lock().map_err(|e| e.to_string())?;
    let tracks = p.get_tracks().map_err(|e| e.to_string())?;
    Ok(tracks
        .into_iter()
        .map(|t| TrackInfoResponse {
            index: t.index,
            kind: t.kind,
            title: t.title,
            lang: t.lang,
            selected: t.selected,
        })
        .collect())
}

#[command]
pub async fn player_select_subtitle_track(
    handle: State<'_, Mutex<MpvPlayer>>,
    index: u32,
) -> Result<(), String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_select_subtitle_track_inner(&mut p, index)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn player_disable_subtitles(handle: State<'_, Mutex<MpvPlayer>>) -> Result<(), String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_disable_subtitles_inner(&mut p)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn player_set_subtitle_visibility(
    handle: State<'_, Mutex<MpvPlayer>>,
    visible: bool,
) -> Result<(), String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_set_subtitle_visibility_inner(&mut p, visible)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn player_set_subtitle_delay(
    handle: State<'_, Mutex<MpvPlayer>>,
    seconds: f64,
) -> Result<(), String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_set_subtitle_delay_inner(&mut p, seconds)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn player_load_subtitle(
    handle: State<'_, Mutex<MpvPlayer>>,
    path: String,
) -> Result<(), String> {
    let mut p = handle.lock().map_err(|e| e.to_string())?;
    frameforge_tauri_plugin_player::player_load_subtitle_inner(&mut p, &path)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn open_subtitle_dialog() -> Result<Option<String>, String> {
    let files = rfd::AsyncFileDialog::new()
        .add_filter("Subtitles", &["srt", "ass", "ssa", "vtt"])
        .add_filter("All", &["*"])
        .pick_file()
        .await;
    Ok(files.map(|f| f.path().to_string_lossy().to_string()))
}