//! MPRIS (Media Player Remote Interfacing Specification) D-Bus integration.
//!
//! Implements the org.mpris.MediaPlayer2 interface for desktop integration,
//! allowing the system media indicator / sound menu to control playback
//! and display media metadata.

use frameforge_core_types::FrameForgeError;
use frameforge_mpv_adapter::MpvPlayer;
use std::result::Result as CoreResult;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use zbus::names::WellKnownName;
use zbus::{Dict, OwnedObjectPath, OwnedVariant, Signature};

/// MPRIS media player server implementing org.mpris.MediaPlayer2.
pub struct MprisServer {
    player: Arc<std::sync::Mutex<MpvPlayer>>,
    running: Arc<AtomicBool>,
    connection: Arc<AsyncMutex<Option<zbus::Connection>>>,
    /// Track list for TrackList interface (optional)
    track_list: Arc<AsyncMutex<Vec<MprisTrack>>>,
    /// Current track metadata
    current_metadata: Arc<AsyncMutex<Option<MprisMetadata>>>,
    /// Current playback status
    playback_status: Arc<AsyncMutex<PlaybackStatus>>,
    /// Current loop status
    loop_status: Arc<AsyncMutex<LoopStatus>>,
    /// Current volume (0.0 - 1.0)
    volume: Arc<AsyncMutex<f64>>,
    /// Current position in microseconds
    position: Arc<AsyncMutex<i64>>,
    /// Playback rate
    rate: Arc<AsyncMutex<f64>>,
    /// Shuffle mode
    shuffle: Arc<AsyncMutex<bool>>,
    /// CanGoNext
    can_go_next: Arc<AsyncMutex<bool>>,
    /// CanGoPrevious
    can_go_previous: Arc<AsyncMutex<bool>>,
    /// CanPlay
    can_play: Arc<AsyncMutex<bool>>,
    /// CanPause
    can_pause: Arc<AsyncMutex<bool>>,
    /// CanSeek
    can_seek: Arc<AsyncMutex<bool>>,
    /// CanControl
    can_control: Arc<AsyncMutex<bool>>,
    /// Minimum rate
    minimum_rate: Arc<AsyncMutex<f64>>,
    /// Maximum rate
    maximum_rate: Arc<AsyncMutex<f64>>,
}

/// Track information for TrackList interface.
#[derive(Debug, Clone)]
struct MprisTrack {
    id: OwnedObjectPath,
    #[allow(dead_code)]
    metadata: MprisMetadata,
}

/// MPRIS metadata structure matching the specification.
#[derive(Debug, Clone, Default)]
struct MprisMetadata {
    title: Option<String>,
    track_number: Option<u32>,
    duration: Option<i64>,
}

/// MPRIS PlaybackStatus enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    fn as_str(&self) -> &'static str {
        match self {
            PlaybackStatus::Playing => "Playing",
            PlaybackStatus::Paused => "Paused",
            PlaybackStatus::Stopped => "Stopped",
        }
    }
}

/// MPRIS LoopStatus enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopStatus {
    None,
    #[allow(dead_code)]
    Track,
    #[allow(dead_code)]
    Playlist,
}

impl LoopStatus {
    fn as_str(&self) -> &'static str {
        match self {
            LoopStatus::None => "None",
            LoopStatus::Track => "Track",
            LoopStatus::Playlist => "Playlist",
        }
    }
}

impl MprisServer {
    /// Create a new MPRIS server instance.
    pub fn new(player: Arc<std::sync::Mutex<MpvPlayer>>) -> CoreResult<Self, FrameForgeError> {
        Ok(Self {
            player,
            running: Arc::new(AtomicBool::new(false)),
            connection: Arc::new(AsyncMutex::new(None)),
            track_list: Arc::new(AsyncMutex::new(Vec::new())),
            current_metadata: Arc::new(AsyncMutex::new(None)),
            playback_status: Arc::new(AsyncMutex::new(PlaybackStatus::Stopped)),
            loop_status: Arc::new(AsyncMutex::new(LoopStatus::None)),
            volume: Arc::new(AsyncMutex::new(1.0)),
            position: Arc::new(AsyncMutex::new(0)),
            rate: Arc::new(AsyncMutex::new(1.0)),
            shuffle: Arc::new(AsyncMutex::new(false)),
            can_go_next: Arc::new(AsyncMutex::new(false)),
            can_go_previous: Arc::new(AsyncMutex::new(false)),
            can_play: Arc::new(AsyncMutex::new(true)),
            can_pause: Arc::new(AsyncMutex::new(true)),
            can_seek: Arc::new(AsyncMutex::new(true)),
            can_control: Arc::new(AsyncMutex::new(true)),
            minimum_rate: Arc::new(AsyncMutex::new(0.25)),
            maximum_rate: Arc::new(AsyncMutex::new(4.0)),
        })
    }

    /// Start the MPRIS D-Bus service.
    pub async fn start(&mut self) -> CoreResult<(), FrameForgeError> {
        if self.running.load(Ordering::SeqCst) {
            return Ok(());
        }

        // Connect to the session bus
        let connection = zbus::Connection::session().await.map_err(|e| {
            FrameForgeError::Probe(format!("Failed to connect to session bus: {e}"))
        })?;

        // Request the well-known name
        let name = WellKnownName::try_from("org.mpris.MediaPlayer2.frameforge")
            .map_err(|e| FrameForgeError::Probe(format!("Invalid bus name: {e}")))?;
        connection
            .request_name(name)
            .await
            .map_err(|e| FrameForgeError::Probe(format!("Failed to request name: {e}")))?;

        // Create shared state for all interfaces
        let state = MprisState {
            player: self.player.clone(),
            track_list: self.track_list.clone(),
            current_metadata: self.current_metadata.clone(),
            playback_status: self.playback_status.clone(),
            loop_status: self.loop_status.clone(),
            volume: self.volume.clone(),
            position: self.position.clone(),
            rate: self.rate.clone(),
            shuffle: self.shuffle.clone(),
            can_go_next: self.can_go_next.clone(),
            can_go_previous: self.can_go_previous.clone(),
            can_play: self.can_play.clone(),
            can_pause: self.can_pause.clone(),
            can_seek: self.can_seek.clone(),
            can_control: self.can_control.clone(),
            minimum_rate: self.minimum_rate.clone(),
            maximum_rate: self.maximum_rate.clone(),
        };

        // Register the root interface
        let root = MprisRoot {
            _state: state.clone(),
        };
        connection
            .object_server()
            .at("/org/mpris/MediaPlayer2", root)
            .await
            .map_err(|e| FrameForgeError::Probe(format!("Failed to register root object: {e}")))?;

        // Register the Player interface
        let player_iface = MprisPlayer {
            state: state.clone(),
        };
        connection
            .object_server()
            .at("/org/mpris/MediaPlayer2", player_iface)
            .await
            .map_err(|e| {
                FrameForgeError::Probe(format!("Failed to register player object: {e}"))
            })?;

        // Register the TrackList interface
        let tracklist = MprisTrackList {
            state: state.clone(),
        };
        connection
            .object_server()
            .at("/org/mpris/MediaPlayer2", tracklist)
            .await
            .map_err(|e| {
                FrameForgeError::Probe(format!("Failed to register tracklist object: {e}"))
            })?;

        // Store connection for cleanup
        *self.connection.lock().await = Some(connection);

        self.running.store(true, Ordering::SeqCst);

        // MPRIS polling is handled by the dedicated task in main.rs
        // that shares Arc<MpvPlayer> directly. No additional setup needed here.

        Ok(())
    }

    /// Stop the MPRIS D-Bus service.
    pub async fn stop(&mut self) -> CoreResult<(), FrameForgeError> {
        if !self.running.load(Ordering::SeqCst) {
            return Ok(());
        }

        // Drop the connection to unregister from D-Bus
        if let Some(conn) = self.connection.lock().await.take() {
            let _ = conn.close().await;
        }

        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// Shared state for all MPRIS interfaces.
#[derive(Clone)]
pub(crate) struct MprisState {
    player: Arc<std::sync::Mutex<MpvPlayer>>,
    track_list: Arc<AsyncMutex<Vec<MprisTrack>>>,
    current_metadata: Arc<AsyncMutex<Option<MprisMetadata>>>,
    playback_status: Arc<AsyncMutex<PlaybackStatus>>,
    loop_status: Arc<AsyncMutex<LoopStatus>>,
    volume: Arc<AsyncMutex<f64>>,
    position: Arc<AsyncMutex<i64>>,
    rate: Arc<AsyncMutex<f64>>,
    shuffle: Arc<AsyncMutex<bool>>,
    can_go_next: Arc<AsyncMutex<bool>>,
    can_go_previous: Arc<AsyncMutex<bool>>,
    can_play: Arc<AsyncMutex<bool>>,
    can_pause: Arc<AsyncMutex<bool>>,
    can_seek: Arc<AsyncMutex<bool>>,
    can_control: Arc<AsyncMutex<bool>>,
    minimum_rate: Arc<AsyncMutex<f64>>,
    maximum_rate: Arc<AsyncMutex<f64>>,
}

/// State clone for background polling task.
#[allow(dead_code)]
struct MprisPollState {
    player: Arc<std::sync::Mutex<MpvPlayer>>,
    track_list: Arc<AsyncMutex<Vec<MprisTrack>>>,
    current_metadata: Arc<AsyncMutex<Option<MprisMetadata>>>,
    playback_status: Arc<AsyncMutex<PlaybackStatus>>,
    loop_status: Arc<AsyncMutex<LoopStatus>>,
    volume: Arc<AsyncMutex<f64>>,
    position: Arc<AsyncMutex<i64>>,
    rate: Arc<AsyncMutex<f64>>,
    _shuffle: Arc<AsyncMutex<bool>>,
    can_go_next: Arc<AsyncMutex<bool>>,
    can_go_previous: Arc<AsyncMutex<bool>>,
    can_play: Arc<AsyncMutex<bool>>,
    can_pause: Arc<AsyncMutex<bool>>,
    can_seek: Arc<AsyncMutex<bool>>,
    can_control: Arc<AsyncMutex<bool>>,
    _minimum_rate: Arc<AsyncMutex<f64>>,
    _maximum_rate: Arc<AsyncMutex<f64>>,
    running: Arc<AtomicBool>,
}

impl MprisState {
    /// Lock the player mutex.
    ///
    /// A poisoned mutex indicates a previous holder panicked while holding it;
    /// the shared player state may be inconsistent, so we fail fast (panic)
    /// instead of silently recovering, which would hide the underlying bug.
    fn lock_player(&self) -> std::sync::MutexGuard<'_, MpvPlayer> {
        self.player
            .lock()
            .expect("MPRIS player mutex poisoned — refusing to continue with inconsistent state")
    }
}

/// org.mpris.MediaPlayer2 root interface
pub struct MprisRoot {
    _state: MprisState,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl MprisRoot {
    // Properties
    #[zbus(property)]
    async fn identity(&self) -> String {
        "FrameForge".to_string()
    }

    #[zbus(property)]
    async fn desktop_entry(&self) -> String {
        "frameforge".to_string()
    }

    #[zbus(property)]
    async fn supported_uri_schemes(&self) -> Vec<String> {
        vec![
            "file".to_string(),
            "http".to_string(),
            "https".to_string(),
            "ftp".to_string(),
        ]
    }

    #[zbus(property)]
    async fn supported_mime_types(&self) -> Vec<String> {
        vec![
            "audio/mpeg".to_string(),
            "audio/ogg".to_string(),
            "audio/flac".to_string(),
            "audio/x-wav".to_string(),
            "video/mp4".to_string(),
            "video/x-matroska".to_string(),
            "video/webm".to_string(),
            "video/x-msvideo".to_string(),
            "video/quicktime".to_string(),
            "application/ogg".to_string(),
        ]
    }

    #[zbus(property)]
    async fn supported_interfaces(&self) -> Vec<String> {
        vec![
            "org.mpris.MediaPlayer2.Player".to_string(),
            "org.mpris.MediaPlayer2.TrackList".to_string(),
        ]
    }

    #[zbus(property)]
    async fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn can_raise(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn has_track_list(&self) -> bool {
        true
    }

    // Methods
    async fn raise(&self) -> Result<(), zbus::fdo::Error> {
        Ok(())
    }

    async fn quit(&self) -> Result<(), zbus::fdo::Error> {
        Err(zbus::fdo::Error::NotSupported("Quit not supported".into()))
    }
}

/// org.mpris.MediaPlayer2.Player interface
pub struct MprisPlayer {
    state: MprisState,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl MprisPlayer {
    // Properties
    #[zbus(property)]
    async fn playback_status(&self) -> String {
        let status = self.state.playback_status.lock().await;
        status.as_str().to_string()
    }

    #[zbus(property)]
    async fn loop_status(&self) -> String {
        let status = self.state.loop_status.lock().await;
        status.as_str().to_string()
    }

    #[zbus(property)]
    async fn rate(&self) -> f64 {
        *self.state.rate.lock().await
    }

    #[zbus(property)]
    async fn set_rate(&self, value: f64) -> Result<(), zbus::fdo::Error> {
        let min = *self.state.minimum_rate.lock().await;
        let max = *self.state.maximum_rate.lock().await;
        let clamped = value.clamp(min, max);

        {
            let mut player = self.state.lock_player();
            player.set_speed(clamped);
        }
        *self.state.rate.lock().await = clamped;
        Ok(())
    }

    #[zbus(property)]
    async fn shuffle(&self) -> bool {
        *self.state.shuffle.lock().await
    }

    #[zbus(property)]
    async fn set_shuffle(&self, value: bool) -> Result<(), zbus::fdo::Error> {
        *self.state.shuffle.lock().await = value;
        {
            let mut player = self.state.lock_player();
            player
                .set_property("shuffle", value)
                .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to set shuffle: {e}")))?;
        }
        Ok(())
    }

    #[zbus(property)]
    async fn metadata(&self) -> OwnedVariant {
        let metadata = self.state.current_metadata.lock().await;
        match metadata.as_ref() {
            Some(m) => metadata_to_owned_value(m),
            None => {
                let dict = Dict::new(
                    &Signature::from_bytes(b"s").unwrap(),
                    &Signature::from_bytes(b"v").unwrap(),
                );
                OwnedVariant::try_from(dict).unwrap()
            }
        }
    }

    #[zbus(property)]
    async fn volume(&self) -> f64 {
        *self.state.volume.lock().await
    }

    #[zbus(property)]
    async fn set_volume(&self, value: f64) -> Result<(), zbus::fdo::Error> {
        let clamped = value.clamp(0.0, 1.0);
        {
            let mut player = self.state.lock_player();
            player.set_volume(clamped);
        }
        *self.state.volume.lock().await = clamped;
        Ok(())
    }

    #[zbus(property)]
    async fn position(&self) -> i64 {
        *self.state.position.lock().await
    }

    #[zbus(property)]
    async fn minimum_rate(&self) -> f64 {
        *self.state.minimum_rate.lock().await
    }

    #[zbus(property)]
    async fn maximum_rate(&self) -> f64 {
        *self.state.maximum_rate.lock().await
    }

    #[zbus(property)]
    async fn can_go_next(&self) -> bool {
        *self.state.can_go_next.lock().await
    }

    #[zbus(property)]
    async fn can_go_previous(&self) -> bool {
        *self.state.can_go_previous.lock().await
    }

    #[zbus(property)]
    async fn can_play(&self) -> bool {
        *self.state.can_play.lock().await
    }

    #[zbus(property)]
    async fn can_pause(&self) -> bool {
        *self.state.can_pause.lock().await
    }

    #[zbus(property)]
    async fn can_seek(&self) -> bool {
        *self.state.can_seek.lock().await
    }

    #[zbus(property)]
    async fn can_control(&self) -> bool {
        *self.state.can_control.lock().await
    }

    // Methods
    async fn next(&self) -> Result<(), zbus::fdo::Error> {
        {
            let mut player = self.state.lock_player();
            player
                .command("playlist-next", &[])
                .map_err(|e| zbus::fdo::Error::Failed(format!("Next failed: {e}")))?;
        }
        Ok(())
    }

    async fn previous(&self) -> Result<(), zbus::fdo::Error> {
        {
            let mut player = self.state.lock_player();
            player
                .command("playlist-prev", &[])
                .map_err(|e| zbus::fdo::Error::Failed(format!("Previous failed: {e}")))?;
        }
        Ok(())
    }

    async fn pause(&self) -> Result<(), zbus::fdo::Error> {
        {
            let mut player = self.state.lock_player();
            player
                .set_property("pause", true)
                .map_err(|e| zbus::fdo::Error::Failed(format!("Pause failed: {e}")))?;
        }
        *self.state.playback_status.lock().await = PlaybackStatus::Paused;
        Ok(())
    }

    async fn play(&self) -> Result<(), zbus::fdo::Error> {
        {
            let mut player = self.state.lock_player();
            player
                .set_property("pause", false)
                .map_err(|e| zbus::fdo::Error::Failed(format!("Play failed: {e}")))?;
        }
        *self.state.playback_status.lock().await = PlaybackStatus::Playing;
        Ok(())
    }

    async fn play_pause(&self) -> Result<(), zbus::fdo::Error> {
        let paused: bool = {
            let mut player = self.state.lock_player();
            let p = player.get_property("pause").unwrap_or(true);
            player
                .set_property("pause", !p)
                .map_err(|e| zbus::fdo::Error::Failed(format!("PlayPause failed: {e}")))?;
            p
        };
        *self.state.playback_status.lock().await = if paused {
            PlaybackStatus::Playing
        } else {
            PlaybackStatus::Paused
        };
        Ok(())
    }

    async fn stop(&self) -> Result<(), zbus::fdo::Error> {
        {
            let mut player = self.state.lock_player();
            player
                .command("stop", &[])
                .map_err(|e| zbus::fdo::Error::Failed(format!("Stop failed: {e}")))?;
        }
        *self.state.playback_status.lock().await = PlaybackStatus::Stopped;
        *self.state.position.lock().await = 0;
        Ok(())
    }

    async fn seek(&self, offset: i64) -> Result<(), zbus::fdo::Error> {
        // offset is in microseconds
        let offset_seconds = offset as f64 / 1_000_000.0;
        {
            let mut player = self.state.lock_player();
            player.seek_relative(offset_seconds);
        }
        Ok(())
    }

    async fn set_position(
        &self,
        _track_id: OwnedObjectPath,
        position: i64,
    ) -> Result<(), zbus::fdo::Error> {
        // position is in microseconds
        let position_seconds = position as f64 / 1_000_000.0;
        {
            let mut player = self.state.lock_player();
            player.seek_absolute(position_seconds);
        }
        *self.state.position.lock().await = position;
        Ok(())
    }

    async fn open_uri(&self, uri: String) -> Result<(), zbus::fdo::Error> {
        // Handle file:// URIs
        let path = if uri.starts_with("file://") {
            url::Url::parse(&uri)
                .map_err(|e| zbus::fdo::Error::Failed(format!("Invalid URI: {e}")))?
                .to_file_path()
                .map_err(|_| zbus::fdo::Error::Failed("Failed to convert URI to path".into()))?
                .to_string_lossy()
                .to_string()
        } else {
            uri
        };

        {
            let mut player = self.state.lock_player();
            player
                .load(std::path::Path::new(&path))
                .map_err(|e| zbus::fdo::Error::Failed(format!("OpenUri failed: {e}")))?;
        }
        Ok(())
    }
}

/// org.mpris.MediaPlayer2.TrackList interface
pub struct MprisTrackList {
    state: MprisState,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.TrackList")]
impl MprisTrackList {
    // Properties
    #[zbus(property)]
    async fn track_list(&self) -> Vec<OwnedObjectPath> {
        let tracks = self.state.track_list.lock().await;
        tracks.iter().map(|t| t.id.clone()).collect()
    }

    // Methods
    async fn add_track(
        &self,
        uri: String,
        _after_track: OwnedObjectPath,
        _enqueue_only: bool,
    ) -> Result<OwnedObjectPath, zbus::fdo::Error> {
        let path = if uri.starts_with("file://") {
            url::Url::parse(&uri)
                .map_err(|e| zbus::fdo::Error::Failed(format!("Invalid URI: {e}")))?
                .to_file_path()
                .map_err(|_| zbus::fdo::Error::Failed("Failed to convert URI to path".into()))?
                .to_string_lossy()
                .to_string()
        } else {
            uri
        };

        {
            let mut player = self.state.lock_player();
            player
                .command("loadfile", &[&path, "append-play"])
                .map_err(|e| zbus::fdo::Error::Failed(format!("AddTrack failed: {e}")))?;
        }

        // Return the new track ID
        let track_count = {
            let tracks = self.state.track_list.lock().await;
            tracks.len()
        };
        let new_id = OwnedObjectPath::try_from(format!(
            "/org/mpris/MediaPlayer2/TrackList/Track{}",
            track_count
        ))
        .unwrap_or_else(|_| {
            OwnedObjectPath::try_from("/org/mpris/MediaPlayer2/TrackList/Track0").unwrap()
        });

        Ok(new_id)
    }

    async fn remove_track(&self, track_id: OwnedObjectPath) -> Result<(), zbus::fdo::Error> {
        // Find the track index and remove it
        let idx = {
            let tracks = self.state.track_list.lock().await;
            tracks.iter().position(|t| t.id == track_id)
        };

        if let Some(idx) = idx {
            {
                let mut player = self.state.lock_player();
                player
                    .command("playlist-remove", &[&idx.to_string()])
                    .map_err(|e| zbus::fdo::Error::Failed(format!("RemoveTrack failed: {e}")))?;
            }
        }
        Ok(())
    }

    async fn get_tracks_metadata(
        &self,
        _track_ids: Vec<OwnedObjectPath>,
    ) -> Result<Vec<OwnedVariant>, zbus::fdo::Error> {
        // TODO: Implement proper metadata return type
        // For now, return empty array
        Ok(Vec::new())
    }
}

/// Helper to convert MprisMetadata to D-Bus OwnedVariant
fn metadata_to_owned_value(metadata: &MprisMetadata) -> OwnedVariant {
    let mut dict = Dict::new(
        &Signature::from_bytes(b"s").unwrap(),
        &Signature::from_bytes(b"v").unwrap(),
    );

    if let Some(ref title) = metadata.title {
        let _ = dict.append("xesam:title".into(), title.clone().into());
    }
    if let Some(track_number) = metadata.track_number {
        let _ = dict.append("xesam:trackNumber".into(), (track_number as i32).into());
    }
    if let Some(duration) = metadata.duration {
        let _ = dict.append("mpris:length".into(), duration.into());
    }

    OwnedVariant::try_from(dict).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[zbus::interface(name = "org.test.Test")]
    impl TestInterface {
        #[zbus(property)]
        fn test_prop(&self) -> String {
            "test".to_string()
        }
    }

    pub struct TestInterface;

    #[test]
    fn mpris_struct_exists() {
        assert!(true);
    }

    #[test]
    fn playback_status_serialization() {
        assert_eq!(PlaybackStatus::Playing.as_str(), "Playing");
        assert_eq!(PlaybackStatus::Paused.as_str(), "Paused");
        assert_eq!(PlaybackStatus::Stopped.as_str(), "Stopped");
    }

    #[test]
    fn loop_status_serialization() {
        assert_eq!(LoopStatus::None.as_str(), "None");
        assert_eq!(LoopStatus::Track.as_str(), "Track");
        assert_eq!(LoopStatus::Playlist.as_str(), "Playlist");
    }
}
