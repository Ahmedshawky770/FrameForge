# Public API

## 7.1 Rust core — crates (pub types/traits, complete)

### core-types
```rust
// Rational timebase and timestamps
pub type Rational = num_rational::Ratio<i64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timebase { pub num: i64, pub den: i64 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timestamp { pub value: i64, pub tb: Timebase }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeRange { pub start: Timestamp, pub end: Timestamp } // end exclusive

// Identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemId(pub u64);

// Cut/segment
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CutMode { KeepSegments, RemoveSegments }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Segment { pub range: TimeRange, pub source_id: SourceId }

// Transitions (for future multi-track)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Transition {
    CrossDissolve { duration: f64 },
    DipToColor { duration: f64, color: String },
    Push { duration: f64, direction: PushDirection },
    Wipe { duration: f64, angle: f64 },
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PushDirection { Left, Right, Up, Down }

// Timeline item (clip instance on a track)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineItem {
    pub id: ItemId,
    pub source_id: SourceId,
    pub name: String,
    pub path: PathBuf,
    pub source_range: TimeRange,
    pub timeline_start: Timestamp,
    pub track_index: u32,
    pub transition_in: Option<Transition>,
    pub transition_out: Option<Transition>,
    pub volume: f32,
    pub opacity: f32,
    pub enabled: bool,
    pub locked: bool,
}

// Tracks
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind { Video, Audio, Subtitle }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub kind: TrackKind,
    pub enabled: bool,
    pub index: Option<u32>,
    pub name: String,
    pub height: f32,
    pub muted: bool,
    pub solo: bool,
    pub locked: bool,
    pub items: Vec<TimelineItem>,
}

// Clips (source media with segments)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Clip {
    pub id: SourceId,
    pub path: PathBuf,
    pub segments: Vec<Segment>,
}

// Export
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExportMode { StreamCopy, Reencode }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Container { Mp4, Mkv, Ts }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExportSettings {
    pub mode: ExportMode,
    pub preset: String,
    pub container: Container,
    pub vcodec: Option<String>, pub acodec: Option<String>,
    pub width: Option<u32>, pub height: Option<u32>,
    pub fps_num: Option<u32>, pub fps_den: Option<u32>,
    pub audio_sample_rate: Option<u32>, pub audio_channels: Option<u32>,
    pub destination: PathBuf,
}

// EDL (project file)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edl {
    pub version: u32,
    pub clips: Vec<Clip>,
    pub tracks: Vec<Track>,
    pub export: ExportSettings,
}

// Errors
#[derive(Debug, Error)]
pub enum FrameForgeError {
    #[error("IO error: {0}")] Io(#[from] std::io::Error),
    #[error("Serde error: {0}")] Serde(#[from] serde_json::Error),
    #[error("Probe error: {0}")] Probe(String),
    #[error("Seek error: {0}")] Seek(String),
    #[error("Export error: {0}")] Export(String),
    #[error("Compatibility error: {0}")] Compatibility(String),
    #[error("Invalid EDL: {0}")] InvalidEdl(String),
    #[error("Hardware not available: {0}")] HwNotAvailable(String),
}
pub type Result<T> = std::result::Result<T, FrameForgeError>;

// FFmpeg initialization guard
pub mod ffmpeg_init {
    pub fn init() { /* Once guard for ffmpeg_next::init() */ }
}
```

### media-probe
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind { Video, Audio, Subtitle, Other }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: u32,
    pub kind: StreamKind,
    pub codec: String,
    pub codec_type: String,
    pub width: Option<u32>, pub height: Option<u32>,
    pub fps_num: Option<u32>, pub fps_den: Option<u32>,
    pub timebase_num: i64, pub timebase_den: i64,
    pub pix_fmt: Option<String>,
    pub sample_rate: Option<u32>, pub channels: Option<u32>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatInfo {
    pub format_name: String,
    pub duration_sec: Option<f64>,
    pub bit_rate: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult { pub streams: Vec<StreamInfo>, pub format: FormatInfo }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyframeEntry { pub pts: Timestamp, pub file_offset: u64 }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct KeyframeIndex { pub entries: Vec<KeyframeEntry> }

impl KeyframeIndex {
    pub fn latest_keyframe_at_or_before(&self, target: &Timestamp) -> Option<&KeyframeEntry>;
}

pub fn probe(path: &Path) -> Result<ProbeResult>;
pub fn build_keyframe_index(path: &Path) -> Result<KeyframeIndex>;
```

### scrub-engine
```rust
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub pts: Timestamp,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // RGBA
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken { /* Arc<AtomicBool> */ }
impl CancelToken {
    pub fn new() -> Self;
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct ScrubResult {
    pub target: Timestamp,
    pub frame: Option<DecodedFrame>,
    pub cancelled: bool,
}

pub struct ScrubSession {
    pub token: CancelToken,
    // ... internal state
}
impl ScrubSession {
    pub fn new() -> Self;
    pub fn open_file(&mut self, path: &Path) -> Result<()>;
    pub fn seek(&mut self, target: Timestamp) -> Result<ScrubResult>;
    pub fn seek_to_file(&mut self, path: &Path, target: Timestamp) -> Result<ScrubResult>;
    pub fn close(&mut self);
    pub fn next_frame(&mut self) -> Result<Option<DecodedFrame>>;
    pub fn prev_frame(&mut self) -> Result<Option<DecodedFrame>>;
}
```

### edit-engine
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapMode { Keyframe, Exact }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedSegment {
    pub source_id: SourceId,
    pub item_id: Option<u64>,
    pub range: TimeRange,
    pub cut_points: Vec<Timestamp>, // keyframe-aligned for copy; exact for re-encode
    pub timeline_start: Timestamp,
    pub track_index: u32,
    pub transition_in: Option<Transition>,
    pub transition_out: Option<Transition>,
    pub volume: f32,
    pub opacity: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergePlan {
    pub order: Vec<SourceId>,
    pub lossless: bool,
    pub reasons: Vec<String>,
    pub alternative: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackPlan {
    pub kind: TrackKind,
    pub index: u32,
    pub name: String,
    pub enabled: bool,
    pub muted: bool,
    pub solo: bool,
    pub items: Vec<PlannedSegment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportPlan {
    pub segments: Vec<PlannedSegment>,
    pub merge_plan: MergePlan,
    pub tracks: Vec<TrackPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub lossless_possible: bool,
    pub reasons: Vec<String>,
    pub alternative: String,
}

pub fn plan_export(edl: &Edl) -> Result<ExportPlan>;
pub fn analyze_compatibility(tracks: &[Track]) -> Result<CompatibilityReport>;
pub fn validate_edl(edl: &Edl) -> Result<()>;
pub fn ripple_edit(track: &mut Track, after_time: Timestamp, delta_seconds: f64);
pub fn insert_item_ripple(track: &mut Track, item: TimelineItem, at_time: Timestamp) -> Result<()>;
pub fn remove_item_ripple(track: &mut Track, item_id: u64) -> Result<()>;
pub fn trim_item(track: &mut Track, item_id: u64, new_start: Option<Timestamp>, new_end: Option<Timestamp>, ripple: bool) -> Result<()>;
```

### export-engine
```rust
pub type ProgressCallback = dyn Fn(f32) + Send + Sync;

#[derive(Debug, Clone, Default)]
pub struct CancelToken { /* Arc<AtomicBool> */ }
impl CancelToken {
    pub fn new() -> Self;
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
}

pub trait ExportExecutor {
    fn run(&mut self, plan: &ExportPlan, progress: &ProgressCallback, cancel: &CancelToken) -> Result<()>;
}

pub struct StreamCopyExecutor;
impl StreamCopyExecutor { pub fn new(clips: Vec<Clip>, destination: PathBuf) -> Self; }

pub struct ReencodeExecutor;
impl ReencodeExecutor {
    pub fn new(clips: Vec<Clip>, destination: PathBuf, encoder_pref: String, quality_preset: String) -> Self;
    pub fn with_normalization(self, width: Option<u32>, height: Option<u32>, fps_num: Option<u32>, fps_den: Option<u32>, pix_fmt: Option<String>, audio_sample_rate: Option<u32>, audio_channels: Option<u32>) -> Self;
}

pub fn disk_space_free(destination: &Path) -> Result<u64>;
pub fn atomic_write<F>(destination: &Path, write: F) -> Result<()> where F: FnMut(&Path, Option<&'static str>) -> Result<()>;
```

### mpv-adapter
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub index: u32,
    pub kind: String, // "video", "audio", "subtitle"
    pub title: Option<String>,
    pub lang: Option<String>,
    pub selected: bool,
}

pub struct MpvPlayer { /* libmpv handle */ }
impl MpvPlayer {
    pub fn new() -> Result<Self>;
    pub fn load(&mut self, path: &Path) -> Result<()>;
    pub fn play_pause(&mut self);
    pub fn seek(&mut self, t: Timestamp);
    pub fn seek_relative(&mut self, seconds: f64);
    pub fn seek_absolute(&mut self, seconds: f64);
    pub fn set_speed(&mut self, f: f64);
    pub fn set_volume(&mut self, f: f64); // 0.0-1.0
    pub fn set_muted(&mut self, muted: bool);
    pub fn step_frame(&mut self);
    pub fn toggle_fullscreen(&mut self);
    pub fn set_fullscreen(&mut self, fullscreen: bool);
    pub fn set_geometry(&mut self, x: i32, y: i32, width: i32, height: i32);
    pub fn center_video_in_window(&mut self, window_width: i32, window_height: i32);
    pub fn load_subtitle(&mut self, path: &Path) -> Result<()>;
    pub fn get_tracks(&self) -> Result<Vec<TrackInfo>>;
    pub fn select_track(&mut self, index: u32, selected: bool) -> Result<()>;
    pub fn select_subtitle_track(&mut self, index: u32) -> Result<()>;
    pub fn disable_subtitles(&mut self) -> Result<()>;
    pub fn set_subtitle_visibility(&mut self, visible: bool) -> Result<()>;
    pub fn set_subtitle_delay(&mut self, seconds: f64) -> Result<()>;
    pub fn set_wid(&mut self, wid: u64); // X11 embedding
    pub fn set_wayland_mode(&mut self); // Wayland native window
    pub fn get_mpv_window_id(&self) -> Option<u64>;
    pub fn set_window_geometry(&mut self, x: i32, y: i32, width: u32, height: u32);
    pub fn time_position(&self) -> Option<f64>;
    pub fn duration(&self) -> Option<f64>;
    pub fn paused(&self) -> bool;
    pub fn volume(&self) -> Option<f64>;
    pub fn speed(&self) -> Option<f64>;
    pub fn command(&mut self, name: &str, args: &[&str]) -> Result<()>;
    pub fn set_property<T: libmpv::SetData>(&mut self, name: &str, value: T) -> Result<()>;
    pub fn get_property<T: libmpv::GetData>(&self, name: &str) -> Result<T>;
    pub fn set_hwdec(&mut self, mode: &str);
    pub fn get_hwdec(&self) -> Option<String>;
    pub fn set_vo(&mut self, vo: &str);
    pub fn get_vo(&self) -> Option<String>;
    pub fn is_running(&self) -> bool;
    pub fn last_error(&self) -> String;
}
```

### hw-diagnostics
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    pub decoder: String,
    pub encoder: String,
    pub interop: String,
    pub copy_back: bool,
    pub hw_rejected_reason: Option<String>,
}
pub fn probe_hw() -> Diagnostics;
```

### tauri-plugin-player (inner functions, called from commands)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerState {
    pub paused: bool,
    pub current_time: f64,
    pub duration: f64,
    pub volume: f64,
    pub playback_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo { /* same as mpv-adapter TrackInfo */ }

pub fn player_load_inner(p: &mut MpvPlayer, src: &str, wid: Option<u64>, is_wayland: bool) -> Result<()>;
pub fn player_play_pause_inner(p: &mut MpvPlayer);
pub fn player_seek_inner(p: &mut MpvPlayer, seconds: f64);
pub fn player_set_speed_inner(p: &mut MpvPlayer, rate: f64);
pub fn player_set_volume_inner(p: &mut MpvPlayer, volume: f64);
pub fn player_set_muted_inner(p: &mut MpvPlayer, muted: bool);
pub fn player_step_frame_inner(p: &mut MpvPlayer);
pub fn player_toggle_fullscreen_inner(p: &mut MpvPlayer);
pub fn player_get_tracks_inner(p: &MpvPlayer) -> Result<Vec<TrackInfo>>;
pub fn player_select_track_inner(p: &mut MpvPlayer, index: u32, selected: bool) -> Result<()>;
pub fn player_select_subtitle_track_inner(p: &mut MpvPlayer, index: u32) -> Result<()>;
pub fn player_disable_subtitles_inner(p: &mut MpvPlayer) -> Result<()>;
pub fn player_set_subtitle_visibility_inner(p: &mut MpvPlayer, visible: bool) -> Result<()>;
pub fn player_set_subtitle_delay_inner(p: &mut MpvPlayer, seconds: f64) -> Result<()>;
pub fn player_load_subtitle_inner(p: &mut MpvPlayer, path: &str) -> Result<()>;
pub fn player_last_error(p: &MpvPlayer) -> String;
pub fn player_set_hwdec(p: &mut MpvPlayer, mode: &str);
pub fn player_get_hwdec(p: &MpvPlayer) -> Option<String>;
pub fn player_set_vo(p: &mut MpvPlayer, vo: &str);
pub fn player_get_vo(p: &MpvPlayer) -> Option<String>;
pub fn player_get_volume(p: &MpvPlayer) -> Option<f64>;
pub fn player_get_speed(p: &MpvPlayer) -> Option<f64>;
```

## 7.2 Tauri Commands (exact list from main.rs)
```rust
// i18n
locale_get() -> LocaleInfo
locale_set(locale: String) -> LocaleInfo
theme_get() -> ThemeInfo
theme_set(theme: String) -> ThemeInfo

// Scrubbing
player_scrub_seek(path: String, seconds: f64) -> ScrubFrameResult
player_scrub_next_frame() -> ScrubFrameResult
player_scrub_prev_frame() -> ScrubFrameResult
scrub_latency() -> LatencyInfo

// Player
player_load(src: String) -> PlayerState
player_play_pause() -> PlayerState
player_seek(seconds: f64) -> PlayerState
player_set_speed(rate: f64) -> PlayerState
player_set_volume(volume: f64) -> PlayerState
player_set_muted(muted: bool) -> PlayerState
player_step_frame() -> PlayerState
player_toggle_fullscreen() -> bool
player_state() -> PlayerState
player_get_last_error() -> String
player_set_hwdec(mode: String) -> ()
player_get_hwdec() -> Option<String>
player_set_vo(vo: String) -> ()
player_get_vo() -> Option<String>
player_get_volume() -> Option<f64>
player_get_speed() -> Option<f64>
player_is_wayland() -> bool
player_resize_child_window(x: i32, y: i32, width: u32, height: u32) -> ()

// Subtitles
player_get_tracks() -> Vec<TrackInfoResponse>
player_select_subtitle_track(index: u32) -> ()
player_disable_subtitles() -> ()
player_set_subtitle_visibility(visible: bool) -> ()
player_set_subtitle_delay(seconds: f64) -> ()
player_load_subtitle(path: String) -> ()
open_subtitle_dialog() -> Option<String>

// Project
project_compatibility(clip_paths: Vec<String>) -> CompatibilityInfo
project_save(edl_json: String, path: String) -> ()
project_load(path: String) -> String
project_export(edl_json: String, output_path: String, encoder_pref: String, quality_preset: String) -> ()
project_export_cancel() -> ()
project_export_progress() -> f32
save_file_dialog() -> Option<String>
save_project_dialog() -> Option<String>
load_project_dialog() -> Option<String>
open_file_dialog() -> Option<String>
open_video_files_dialog() -> Option<Vec<String>>
project_delete_ranges(source_path: Option<String>, duration: f64, delete_ranges: Vec<(f64, f64)>) -> String
media_probe_duration(path: String) -> Option<f64>
```

## 7.3 TypeScript — `PlayerBackend` interface (web component)
```ts
export interface TimeRange { start: number; end: number } // seconds
export interface Track { kind: 'video'|'audio'|'subtitle'; enabled: boolean; index?: number }
export interface ExportSettings { mode: 'stream-copy'|'reencode'; preset: string; container: 'mp4'|'mkv'|'ts'; destination: string }

export interface PlayerBackend {
  // lifecycle
  load(src: string): Promise<void>;
  play(): Promise<void>; pause(): Promise<void>; togglePlay(): Promise<void>;
  seek(seconds: number): Promise<void>;
  setPlaybackRate(rate: number): Promise<void>;
  setVolume(volume: number): Promise<void>;
  setMuted(muted: boolean): Promise<void>;
  requestFullscreen(): Promise<void>; exitFullscreen(): Promise<void>;
  stepFrame(): Promise<void>; // frame step forward

  // tracks
  getTracks(): Promise<Track[]>; selectTrack(kind: Track['kind'], index: number): Promise<void>;

  // events (EventTarget-style)
  addEventListener(type: 'play'|'pause'|'seeked'|'ended'|'error'|'timeupdate'|'loadedmetadata', cb: (ev: Event) => void): void;
  removeEventListener(type: string, cb: (ev: Event) => void): void;

  // read-only state
  readonly duration: number;
  readonly currentTime: number;
  readonly paused: boolean;
  readonly readyState: number;
  readonly supportsLosslessCut: boolean;
  readonly locale: string; // 'en' | 'ar', runtime switchable
  setLocale(locale: string): void;
}

export class MediaPlayerElement extends HTMLElement implements PlayerBackend { /* ... */ }
customElements.define('media-player', MediaPlayerElement);
```

## 7.4 Usage examples

### (a) Website — playback only
```html
<media-player src="clip.mp4" locale="ar"></media-player>
<script>
  const player = document.querySelector('media-player');
  player.addEventListener('loadedmetadata', () => {
    console.log('duration', player.duration);
  });
  await player.play();
</script>
```
Capability tier: native `<video>` -> MSE remux -> WebCodecs+WASM demuxer -> "unsupported, use desktop app". No cut/merge in browser.

### (b) Third-party desktop app embedding the library
```rust
use frameforge_core_types::{Edl, ExportSettings, ExportMode};
use frameforge_edit_engine::{plan_export, analyze_compatibility};
use frameforge_export_engine::{StreamCopyExecutor, ReencodeExecutor, ExportExecutor};
use frameforge_mpv_adapter::MpvPlayer;

let mut mpv = MpvPlayer::new()?;
mpv.load(std::path::Path::new("video.mp4"))?;
mpv.play();

// plan an export
let edl: Edl = serde_json::from_str(&project_json)?;
let plan = plan_export(&edl)?;
if !plan.merge_plan.lossless {
    println!("re-encode needed: {:?}", plan.merge_plan.reasons);
}

let mut exec: Box<dyn ExportExecutor> = if plan.merge_plan.lossless {
    Box::new(StreamCopyExecutor::new(edl.clips, output_path))
} else {
    Box::new(ReencodeExecutor::new(edl.clips, output_path, "x264".into(), "medium".into()))
};
exec.run(&plan, |p| println!("{:.1}%", p), cancel_token)?;
```

### (c) Headless CLI
```bash
# Probe a file
frameforge-cli probe input.mp4

# Build keyframe index
frameforge-cli build-index input.mp4

# Diagnose hardware
frameforge-cli diagnose-hw

# Export with segments
frameforge-cli export --input input.mp4 --output out.mp4 --segments "0,5 10,15"

# Validate EDL
frameforge-cli validate-edl project.json
```

## 7.5 EDL JSON schema (complete)
```json
{
  "version": 1,
  "clips": [
    {
      "id": 1,
      "path": "/home/user/video.mp4",
      "segments": [{"start": 0.0, "end": 5.0}, {"start": 10.0, "end": 15.0}]
    }
  ],
  "tracks": [
    {"kind":"video","enabled":true,"index":0,"name":"Video 1","height":80,"muted":false,"solo":false,"locked":false,"items":[]},
    {"kind":"audio","enabled":true,"index":1,"name":"Audio 1","height":60,"muted":false,"solo":false,"locked":false,"items":[]}
  ],
  "export": {
    "mode": "stream-copy",
    "preset": "fast",
    "container": "mp4",
    "vcodec": null,
    "acodec": null,
    "width": null,
    "height": null,
    "fps_num": null,
    "fps_den": null,
    "audio_sample_rate": null,
    "audio_channels": null,
    "destination": "/home/user/out.mp4"
  }
}
```
Times are seconds (display); internally stored as rational timebase timestamps. `start` inclusive, `end` exclusive.

## Assumptions
- serde + serde_json used for EDL serialization.
- Fluent/ICU4X for TS i18n; selection to be confirmed by bundle-size measurement.
- `frameforge_*` crate names are placeholders pending final crate naming.