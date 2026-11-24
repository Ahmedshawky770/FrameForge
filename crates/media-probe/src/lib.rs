//! media-probe: probe input files and build keyframe indexes using ffmpeg-next.

use ffmpeg_next::Rational;
use frameforge_core_types::{FrameForgeError, Timebase, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Process-wide keyframe-index cache.
///
/// Building an index requires reading the packet table of the whole file, so
/// doing it once per planned segment turns an N-segment timeline into N full
/// file scans. Entries are keyed by path and validated against the file's
/// length + mtime, so an edited/replaced file is rescanned instead of serving
/// a stale index.
static KEYFRAME_CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();

#[derive(Debug)]
struct CacheEntry {
    len: u64,
    mtime_nanos: u128,
    /// Upper bound (seconds) the cached index covers; `None` = whole file.
    limit: Option<f64>,
    index: Arc<KeyframeIndex>,
}

fn keyframe_cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    KEYFRAME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop all cached keyframe indexes. Useful after batch edits to source media.
pub fn clear_keyframe_index_cache() {
    if let Ok(mut cache) = keyframe_cache().lock() {
        cache.clear();
    }
}

/// `(len, mtime)` fingerprint used to invalidate cache entries.
fn file_fingerprint(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: u32,
    pub kind: StreamKind,
    pub codec: String,
    pub codec_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    pub timebase_num: i64,
    pub timebase_den: i64,
    pub pix_fmt: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatInfo {
    pub format_name: String,
    pub duration_sec: Option<f64>,
    pub bit_rate: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub streams: Vec<StreamInfo>,
    pub format: FormatInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyframeEntry {
    pub pts: Timestamp,
    pub file_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct KeyframeIndex {
    pub entries: Vec<KeyframeEntry>,
}

impl KeyframeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Latest keyframe with pts <= target, or None.
    ///
    /// `entries` are kept sorted by pts, so this is an O(log n) binary search
    /// instead of a full scan.
    pub fn latest_keyframe_at_or_before(&self, target: &Timestamp) -> Option<&KeyframeEntry> {
        let t = target.to_seconds();
        let idx = self.entries.partition_point(|e| e.pts.to_seconds() <= t);
        if idx == 0 {
            None
        } else {
            self.entries.get(idx - 1)
        }
    }

    /// First keyframe with pts >= target, or None.
    pub fn first_keyframe_at_or_after(&self, target: &Timestamp) -> Option<&KeyframeEntry> {
        let t = target.to_seconds();
        let idx = self.entries.partition_point(|e| e.pts.to_seconds() < t);
        self.entries.get(idx)
    }

    /// First keyframe strictly after `target` (never returns a keyframe at or
    /// before the target). Useful for finding the end of the GOP that contains
    /// a cut point.
    pub fn first_keyframe_after(&self, target: &Timestamp) -> Option<&KeyframeEntry> {
        let t = target.to_seconds();
        let idx = self.entries.partition_point(|e| e.pts.to_seconds() <= t);
        self.entries.get(idx)
    }

    /// Latest keyframe strictly before `target` (never returns a keyframe at or
    /// after the target). Useful for finding the start of the GOP that contains
    /// a cut point.
    pub fn last_keyframe_before(&self, target: &Timestamp) -> Option<&KeyframeEntry> {
        let t = target.to_seconds();
        let idx = self.entries.partition_point(|e| e.pts.to_seconds() < t);
        if idx == 0 {
            None
        } else {
            self.entries.get(idx - 1)
        }
    }
}

fn stream_kind(codec_type: ffmpeg_next::media::Type) -> StreamKind {
    match codec_type {
        ffmpeg_next::media::Type::Video => StreamKind::Video,
        ffmpeg_next::media::Type::Audio => StreamKind::Audio,
        ffmpeg_next::media::Type::Subtitle => StreamKind::Subtitle,
        _ => StreamKind::Other,
    }
}

fn codec_name(codecpar: &ffmpeg_next::codec::Parameters) -> String {
    codecpar.id().name().to_string()
}

fn probe_stream(stream: &ffmpeg_next::Stream) -> StreamInfo {
    let codecpar = stream.parameters();
    let time_base = stream.time_base();
    // Use the container's declared (real) base frame rate (r_frame_rate)
    // rather than avg_frame_rate, which for MPEG-TS / VFR-ish streams is
    // derived from packet timing and often degenerates into an unreduced,
    // huge rational that compares unequal to a plain 25/1 even when the
    // content is a steady 25 fps. r_frame_rate is the canonical value
    // ffmpeg reports (e.g. 25/1) and is what compatibility / re-encode
    // decisions should be based on.
    let fps = stream.rate();

    // Read width/height/format/sample_rate/channels via unsafe FFI
    // SAFETY: codecpar is a reference to ffmpeg_next::codec::Parameters obtained from
    // stream.parameters(), which borrows the InputContext (ictx). The InputContext is
    // alive for the duration of this function (it's created at the start of probe() and
    // not dropped until the end). The pointer returned by as_ptr() is guaranteed to be
    // valid (non-null) by ffmpeg-next's API contract for a valid Parameters object.
    // We check for null defensively. The dereference only reads simple integer fields
    // (width, height, format, sample_rate, channels) which are valid for the lifetime
    // of the Parameters object.
    let (width, height, pix_fmt, sample_rate, channels) = unsafe {
        let params_ptr = codecpar.as_ptr();
        if params_ptr.is_null() {
            (None, None, None, None, None)
        } else {
            let p = &*params_ptr;
            let w = if p.width != 0 {
                Some(p.width as u32)
            } else {
                None
            };
            let h = if p.height != 0 {
                Some(p.height as u32)
            } else {
                None
            };
            let fmt = if p.format >= 0 {
                Some(format!("{:?}", p.format))
            } else {
                None
            };
            let sr = if p.sample_rate != 0 {
                Some(p.sample_rate as u32)
            } else {
                None
            };
            let ch = if p.channels != 0 {
                Some(p.channels as u32)
            } else {
                None
            };
            (w, h, fmt, sr, ch)
        }
    };

    StreamInfo {
        index: stream.index() as u32,
        kind: stream_kind(codecpar.medium()),
        codec: codec_name(&codecpar),
        codec_type: format!("{:?}", codecpar.medium()),
        width,
        height,
        fps_num: if fps.numerator() != 0 {
            Some(fps.numerator() as u32)
        } else {
            None
        },
        fps_den: if fps.denominator() != 0 {
            Some(fps.denominator() as u32)
        } else {
            None
        },
        timebase_num: time_base.0 as i64,
        timebase_den: time_base.1 as i64,
        pix_fmt,
        sample_rate,
        channels,
        profile: None,
    }
}

/// Probe a media file and return stream + format info.
pub fn probe(path: &Path) -> Result<ProbeResult, FrameForgeError> {
    ffmpeg_next::init().map_err(|e| FrameForgeError::Probe(format!("ffmpeg init failed: {e}")))?;

    let ictx = ffmpeg_next::format::input(path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

    let mut streams = Vec::new();
    for stream in ictx.streams() {
        streams.push(probe_stream(&stream));
    }

    let duration_sec = unsafe {
        let p = ictx.as_ptr();
        if p.is_null() {
            None
        } else {
            let d = (*p).duration;
            if d == ffmpeg_next::ffi::AV_NOPTS_VALUE {
                None
            } else {
                Some(d as f64 / 1_000_000.0)
            }
        }
    };
    let bit_rate = unsafe {
        let p = ictx.as_ptr();
        if p.is_null() {
            None
        } else {
            let br = (*p).bit_rate;
            if br == 0 {
                None
            } else {
                Some(br as u64)
            }
        }
    };

    Ok(ProbeResult {
        streams,
        format: FormatInfo {
            format_name: ictx.format().name().to_string(),
            duration_sec,
            bit_rate,
        },
    })
}

/// Build a keyframe index for the whole file by reading its packet table.
pub fn build_keyframe_index(path: &Path) -> Result<KeyframeIndex, FrameForgeError> {
    scan_keyframes(path, None)
}

/// Build a keyframe index covering `[0, max_seconds]`.
///
/// The scan stops right after the first keyframe at/after `max_seconds`, so
/// trimming a few seconds out of a long file no longer reads the whole file.
pub fn build_keyframe_index_until(
    path: &Path,
    max_seconds: f64,
) -> Result<KeyframeIndex, FrameForgeError> {
    scan_keyframes(path, Some(max_seconds))
}

/// Return a cached index for `path` if it is valid for `max_seconds`.
///
/// A cached entry is usable when the file fingerprint (len + mtime) matches and
/// the index was built to at least the requested limit.
fn cache_get(path: &Path, max_seconds: Option<f64>) -> Option<Arc<KeyframeIndex>> {
    let (len, mtime_nanos) = file_fingerprint(path)?;
    let cache = keyframe_cache().lock().ok()?;
    let entry = cache.get(path)?;
    let covers = match (entry.limit, max_seconds) {
        (None, _) => true,
        (Some(have), Some(want)) => have >= want,
        (Some(_), None) => false,
    };
    if entry.len == len && entry.mtime_nanos == mtime_nanos && covers {
        Some(Arc::clone(&entry.index))
    } else {
        None
    }
}

/// Cached variant of [`build_keyframe_index_until`].
///
/// Reuses a previously built index for the same path when the file is unchanged
/// and the cached index already covers `max_seconds`. Returns an `Arc` so
/// callers share one allocation across every segment of a plan.
pub fn build_keyframe_index_cached(
    path: &Path,
    max_seconds: Option<f64>,
) -> Result<Arc<KeyframeIndex>, FrameForgeError> {
    if let Some(index) = cache_get(path, max_seconds) {
        return Ok(index);
    }

    let index = Arc::new(scan_keyframes(path, max_seconds)?);

    if let Some((len, mtime_nanos)) = file_fingerprint(path) {
        if let Ok(mut cache) = keyframe_cache().lock() {
            cache.insert(
                path.to_path_buf(),
                CacheEntry {
                    len,
                    mtime_nanos,
                    limit: max_seconds,
                    index: Arc::clone(&index),
                },
            );
        }
    }

    Ok(index)
}

/// Read packets and collect keyframes for the video stream.
///
/// When `limit` is set the scan stops after the first keyframe at/after the
/// limit, which keeps enough entries for both start- and end-snapping.
fn scan_keyframes(path: &Path, limit: Option<f64>) -> Result<KeyframeIndex, FrameForgeError> {
    ffmpeg_next::init().map_err(|e| FrameForgeError::Probe(format!("ffmpeg init failed: {e}")))?;

    let mut ictx = ffmpeg_next::format::input(path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

    // First pass: find video stream index and its time base
    let mut video_stream_idx: Option<usize> = None;
    let mut time_base: Rational = Rational(1, 1000);
    for stream in ictx.streams() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            video_stream_idx = Some(stream.index());
            time_base = stream.time_base();
            break;
        }
    }

    let video_stream_idx =
        video_stream_idx.ok_or_else(|| FrameForgeError::Probe("no video stream found".into()))?;

    let tb_num = time_base.0 as i64;
    let tb_den = time_base.1 as i64;

    let mut entries = Vec::new();
    for (stream, packet) in ictx.packets() {
        if stream.index() != video_stream_idx {
            continue;
        }
        if packet.is_key() {
            let pts = packet.pts().unwrap_or_else(|| packet.dts().unwrap_or(0));
            let seconds = pts as f64 * (tb_num as f64 / tb_den as f64);
            entries.push(KeyframeEntry {
                pts: Timestamp::new(pts, Timebase::new(tb_num, tb_den)),
                file_offset: packet.position().max(0) as u64,
            });
            // Keep the first keyframe at/after the limit so end-snapping still
            // has an upper bound, then stop reading.
            if let Some(limit) = limit {
                if seconds > limit {
                    break;
                }
            }
        }
    }

    // Binary search (latest/first) assumes ascending pts. Packets are in decode
    // order, so sort and drop duplicate timestamps defensively.
    entries.sort_by(|a, b| {
        a.pts
            .to_seconds()
            .partial_cmp(&b.pts.to_seconds())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    entries.dedup_by(|a, b| (a.pts.to_seconds() - b.pts.to_seconds()).abs() < 1e-9);

    Ok(KeyframeIndex { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_index_empty() {
        let idx = KeyframeIndex::new();
        assert!(idx.entries.is_empty());
    }

    #[test]
    #[ignore = "requires test-media/test-video.mp4; run manually with --test media-probe -- --ignored"]
    fn probe_real_file() {
        let path = Path::new("test-media/test-video.mp4");
        if !path.exists() {
            return;
        }
        let result = probe(path).expect("probe should succeed on real file");
        assert!(
            !result.streams.is_empty(),
            "should have at least one stream"
        );
        assert!(!result.format.format_name.is_empty());
        let video = result.streams.iter().find(|s| s.kind == StreamKind::Video);
        assert!(video.is_some(), "should have a video stream");
        let v = video.unwrap();
        assert!(v.width.is_some() && v.height.is_some());
    }

    #[test]
    #[ignore = "requires test-media/test-video.mp4; run manually with --test media-probe -- --ignored"]
    fn build_keyframe_index_real_file() {
        let path = Path::new("test-media/test-video.mp4");
        if !path.exists() {
            return;
        }
        let idx = build_keyframe_index(path).expect("index build should succeed");
        // Keyframe index may be empty for some files; just ensure it doesn't crash.
        let _ = idx.entries.len();
    }

    fn tb() -> Timebase {
        Timebase::new(1, 1000)
    }

    fn idx_at(seconds: &[i64]) -> KeyframeIndex {
        KeyframeIndex {
            entries: seconds
                .iter()
                .map(|&s| KeyframeEntry {
                    pts: Timestamp::new(s, tb()),
                    file_offset: 0,
                })
                .collect(),
        }
    }

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::new(seconds, tb())
    }

    #[test]
    fn latest_keyframe_at_or_before_binary_search() {
        let idx = idx_at(&[0, 40, 80, 120, 160]);

        // Exact hit.
        assert_eq!(
            idx.latest_keyframe_at_or_before(&ts(80)).map(|k| k.pts),
            Some(ts(80))
        );
        // Between keyframes -> previous keyframe.
        assert_eq!(
            idx.latest_keyframe_at_or_before(&ts(100)).map(|k| k.pts),
            Some(ts(80))
        );
        // Before the first keyframe -> None.
        assert!(idx.latest_keyframe_at_or_before(&ts(-1)).is_none());
        // After the last keyframe -> last keyframe.
        assert_eq!(
            idx.latest_keyframe_at_or_before(&ts(999)).map(|k| k.pts),
            Some(ts(160))
        );
        // Empty index.
        assert!(KeyframeIndex::new()
            .latest_keyframe_at_or_before(&ts(10))
            .is_none());
    }

    #[test]
    fn first_keyframe_at_or_after_binary_search() {
        let idx = idx_at(&[0, 40, 80, 120, 160]);

        assert_eq!(
            idx.first_keyframe_at_or_after(&ts(80)).map(|k| k.pts),
            Some(ts(80))
        );
        assert_eq!(
            idx.first_keyframe_at_or_after(&ts(81)).map(|k| k.pts),
            Some(ts(120))
        );
        assert_eq!(
            idx.first_keyframe_at_or_after(&ts(0)).map(|k| k.pts),
            Some(ts(0))
        );
        // Past the end -> None.
        assert!(idx.first_keyframe_at_or_after(&ts(161)).is_none());
        // Empty index.
        assert!(KeyframeIndex::new()
            .first_keyframe_at_or_after(&ts(0))
            .is_none());
    }

    fn temp_media(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "frameforge_kf_{}_{}_{}.bin",
            name,
            std::process::id(),
            n
        ));
        std::fs::write(&path, b"not a real video").unwrap();
        path
    }

    #[test]
    fn cache_reuses_entry_when_file_unchanged_and_limit_covered() {
        let path = temp_media("reuse");
        let (len, mtime_nanos) = file_fingerprint(&path).expect("fingerprint");
        let shared = Arc::new(idx_at(&[0, 40, 80]));

        keyframe_cache().lock().unwrap().insert(
            path.clone(),
            CacheEntry {
                len,
                mtime_nanos,
                limit: None,
                index: Arc::clone(&shared),
            },
        );

        let cached = cache_get(&path, Some(10.0)).expect("cache hit");
        assert!(Arc::ptr_eq(&cached, &shared));
        // A request for the whole file is also served by a whole-file entry.
        assert!(Arc::ptr_eq(&cache_get(&path, None).unwrap(), &shared));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cache_rejects_entry_built_for_a_shorter_limit() {
        let path = temp_media("limit");
        let (len, mtime_nanos) = file_fingerprint(&path).expect("fingerprint");
        let short = Arc::new(idx_at(&[0, 10]));

        keyframe_cache().lock().unwrap().insert(
            path.clone(),
            CacheEntry {
                len,
                mtime_nanos,
                limit: Some(10.0),
                index: Arc::clone(&short),
            },
        );

        // Requesting more than the cached limit must miss and force a rescan.
        assert!(cache_get(&path, Some(20.0)).is_none());
        // A request within the cached limit is served from cache.
        assert!(Arc::ptr_eq(&cache_get(&path, Some(5.0)).unwrap(), &short));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cache_misses_when_file_changes() {
        let path = temp_media("changed");
        let (len, mtime_nanos) = file_fingerprint(&path).expect("fingerprint");
        let stale = Arc::new(idx_at(&[0]));

        keyframe_cache().lock().unwrap().insert(
            path.clone(),
            CacheEntry {
                len: len + 1,
                mtime_nanos,
                limit: None,
                index: stale,
            },
        );

        // Length mismatch invalidates the entry.
        assert!(cache_get(&path, None).is_none());

        let _ = std::fs::remove_file(&path);
    }
}
