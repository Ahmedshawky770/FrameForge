//! Core types: rational timebase, timestamps, EDL, errors.
//! Pure Rust — no C dependencies. Compiles headless on any platform.

use num_rational::Ratio;
use serde::{Deserialize, Serialize};
use std::fmt;
pub use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

/// A rational number (num/den) used for timebases and frame rates.
pub type Rational = Ratio<i64>;

/// A rational timebase: timestamp.value / timebase = seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timebase {
    pub num: i64,
    pub den: i64,
}

impl Timebase {
    pub const fn new(num: i64, den: i64) -> Self {
        Self { num, den }
    }
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

impl Default for Timebase {
    fn default() -> Self {
        Self { num: 1, den: 1000 }
    }
}

impl fmt::Display for Timebase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// A timestamp expressed in a given timebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timestamp {
    pub value: i64,
    pub tb: Timebase,
}

impl Timestamp {
    pub const fn new(value: i64, tb: Timebase) -> Self {
        Self { value, tb }
    }

    /// Convert to seconds (f64). Lossy — only for display/UI.
    pub fn to_seconds(self) -> f64 {
        (self.value as f64) * self.tb.as_f64()
    }

    /// Convert seconds to a Timestamp in the given timebase (rounded).
    pub fn from_seconds(seconds: f64, tb: Timebase) -> Self {
        let value = (seconds / tb.as_f64()).round() as i64;
        Self { value, tb }
    }

    pub fn seconds_in_tb(self, tb: Timebase) -> f64 {
        self.to_seconds() / tb.as_f64()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}/{}", self.value, self.tb.num, self.tb.den)
    }
}

/// A half-open range [start, end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Timestamp,
    pub end: Timestamp,
}

impl TimeRange {
    pub const fn new(start: Timestamp, end: Timestamp) -> Self {
        Self { start, end }
    }

    pub fn duration_seconds(self) -> f64 {
        self.end.to_seconds() - self.start.to_seconds()
    }

    pub fn contains(self, t: Timestamp) -> bool {
        // Compare by converting both to the start's timebase (lossy but fine for UI).
        let s = self.start.to_seconds();
        let e = self.end.to_seconds();
        let v = t.to_seconds();
        v >= s && v < e
    }
}

/// Unique identifier for a source clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub u64);

/// Unique identifier for a timeline item (clip instance on a track).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemId(pub u64);

impl ItemId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().as_u128() as u64)
    }
}

impl Default for ItemId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CutMode {
    KeepSegments,
    RemoveSegments,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Segment {
    pub range: TimeRange,
    pub source_id: SourceId,
}

/// Transition types between timeline items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Transition {
    /// Cross dissolve between two clips
    CrossDissolve { duration: f64 },
    /// Dip to color (usually black)
    DipToColor { duration: f64, color: String },
    /// Push transition (slide)
    Push { duration: f64, direction: PushDirection },
    /// Wipe transition
    Wipe { duration: f64, angle: f64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PushDirection {
    Left,
    Right,
    Up,
    Down,
}

/// A timeline item (clip instance) placed on a track at a specific time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineItem {
    pub id: ItemId,
    pub source_id: SourceId,
    pub name: String,
    pub path: PathBuf,
    /// Source time range (in/out points in source media)
    pub source_range: TimeRange,
    /// Timeline position (where this item starts on the track)
    pub timeline_start: Timestamp,
    /// Track index this item belongs to
    pub track_index: u32,
    /// Optional transition at the start of this item
    pub transition_in: Option<Transition>,
    /// Optional transition at the end of this item
    pub transition_out: Option<Transition>,
    /// Volume gain for audio (1.0 = normal)
    pub volume: f32,
    /// Opacity for video (1.0 = opaque)
    pub opacity: f32,
    /// Whether this item is enabled/visible
    pub enabled: bool,
    /// Locked items cannot be moved/edited
    pub locked: bool,
}

impl TimelineItem {
    /// Get the timeline end position (timeline_start + source_range duration)
    pub fn timeline_end(&self) -> Timestamp {
        let dur = self.source_range.duration_seconds();
        Timestamp::from_seconds(self.timeline_start.to_seconds() + dur, self.timeline_start.tb)
    }

    /// Get duration in seconds
    pub fn duration_seconds(&self) -> f64 {
        self.source_range.duration_seconds()
    }

    /// Check if a timeline time falls within this item
    pub fn contains_time(&self, t: Timestamp) -> bool {
        let start = self.timeline_start.to_seconds();
        let end = self.timeline_end().to_seconds();
        let v = t.to_seconds();
        v >= start && v < end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub kind: TrackKind,
    pub enabled: bool,
    pub index: Option<u32>,
    pub name: String,
    /// Track height multiplier (for UI)
    pub height: f32,
    /// Mute track (audio only)
    pub muted: bool,
    /// Solo track (audio only)
    pub solo: bool,
    /// Lock track (prevent edits)
    pub locked: bool,
    /// Items on this track (ordered by timeline_start)
    pub items: Vec<TimelineItem>,
}

impl Track {
    pub fn new(kind: TrackKind, index: u32, name: String) -> Self {
        Self {
            kind,
            enabled: true,
            index: Some(index),
            name,
            height: 1.0,
            muted: false,
            solo: false,
            locked: false,
            items: Vec::new(),
        }
    }

    /// Get items sorted by timeline position
    pub fn sorted_items(&self) -> Vec<&TimelineItem> {
        let mut items: Vec<_> = self.items.iter().collect();
        items.sort_by(|a, b| a.timeline_start.to_seconds().partial_cmp(&b.timeline_start.to_seconds()).unwrap());
        items
    }

    /// Find item at a specific timeline position
    pub fn item_at_time(&self, t: Timestamp) -> Option<&TimelineItem> {
        self.items.iter().find(|item| item.contains_time(t))
    }

    /// Find item by ID
    pub fn item_by_id(&self, id: ItemId) -> Option<&TimelineItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Find mutable item by ID
    pub fn item_by_id_mut(&mut self, id: ItemId) -> Option<&mut TimelineItem> {
        self.items.iter_mut().find(|item| item.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Clip {
    pub id: SourceId,
    pub path: PathBuf,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExportMode {
    StreamCopy,
    Reencode,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Container {
    Mp4,
    Mkv,
    Ts,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExportSettings {
    pub mode: ExportMode,
    pub preset: String,
    pub container: Container,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    pub audio_sample_rate: Option<u32>,
    pub audio_channels: Option<u32>,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edl {
    pub version: u32,
    pub clips: Vec<Clip>,
    pub tracks: Vec<Track>,
    pub export: ExportSettings,
}

impl Default for Edl {
    fn default() -> Self {
        Self {
            version: 1,
            clips: Vec::new(),
            tracks: vec![
                Track {
                    kind: TrackKind::Video,
                    enabled: true,
                    index: Some(0),
                    name: "Video 1".into(),
                    height: 80.0,
                    muted: false,
                    solo: false,
                    locked: false,
                    items: Vec::new(),
                },
                Track {
                    kind: TrackKind::Audio,
                    enabled: true,
                    index: Some(1),
                    name: "Audio 1".into(),
                    height: 60.0,
                    muted: false,
                    solo: false,
                    locked: false,
                    items: Vec::new(),
                },
            ],
            export: ExportSettings {
                mode: ExportMode::StreamCopy,
                preset: "fast".into(),
                container: Container::Mp4,
                vcodec: None,
                acodec: None,
                width: None,
                height: None,
                fps_num: None,
                fps_den: None,
                audio_sample_rate: None,
                audio_channels: None,
                destination: PathBuf::from("out.mp4"),
            },
        }
    }
}

/// Core error type.
#[derive(Debug, Error)]
pub enum FrameForgeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Probe error: {0}")]
    Probe(String),
    #[error("Seek error: {0}")]
    Seek(String),
    #[error("Export error: {0}")]
    Export(String),
    #[error("Compatibility error: {0}")]
    Compatibility(String),
    #[error("Invalid EDL: {0}")]
    InvalidEdl(String),
    #[error("Hardware not available: {0}")]
    HwNotAvailable(String),
}

pub type Result<T> = std::result::Result<T, FrameForgeError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_to_seconds() {
        let tb = Timebase::new(1, 1000);
        let t = Timestamp::new(1500, tb);
        assert!((t.to_seconds() - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn time_range_contains() {
        let tb = Timebase::new(1, 1000);
        let r = TimeRange::new(Timestamp::new(0, tb), Timestamp::new(5000, tb));
        assert!(r.contains(Timestamp::new(2500, tb)));
        assert!(!r.contains(Timestamp::new(5000, tb))); // end exclusive
    }

    #[test]
    fn edl_default_version() {
        let edl = Edl::default();
        assert_eq!(edl.version, 1);
    }
}

/// FFmpeg initialization guard — ensures `ffmpeg_next::init()` is called exactly once.
pub mod ffmpeg_init {
    use std::sync::Once;

    static INIT: Once = Once::new();

    /// Initialize FFmpeg. Safe to call multiple times; only the first call has effect.
    pub fn init() {
        INIT.call_once(|| {
            // SAFETY: ffmpeg_next::init() is idempotent and must be called before any other
            // ffmpeg_next functions. The Once guard ensures it runs exactly once per process.
            let _ = ffmpeg_next::init();
        });
    }
}
