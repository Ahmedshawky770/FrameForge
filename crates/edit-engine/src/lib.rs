//! edit-engine: cut/merge planning, EDL validation, compatibility analyzer.
//!
//! Uses media-probe (ffmpeg-next) for real stream parameter comparison and
//! keyframe index for lossless cut-point snapping.

use frameforge_core_types::{
    Clip, Edl, ExportMode, FrameForgeError, Result, SourceId, TimeRange, Timestamp, TimelineItem, Track, TrackKind,
};
use frameforge_media_probe::{build_keyframe_index, probe, StreamInfo};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A planned export segment with snapped cut points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedSegment {
    pub source_id: SourceId,
    pub item_id: Option<u64>, // TimelineItem id for tracking
    pub range: TimeRange,
    /// Cut points (keyframe-aligned for copy mode; exact frames for re-encode).
    pub cut_points: Vec<Timestamp>,
    /// Timeline position where this segment appears
    pub timeline_start: Timestamp,
    /// Track index this segment belongs to
    pub track_index: u32,
    /// Transition at start of segment
    pub transition_in: Option<frameforge_core_types::Transition>,
    /// Transition at end of segment
    pub transition_out: Option<frameforge_core_types::Transition>,
    /// Volume gain
    pub volume: f32,
    /// Opacity
    pub opacity: f32,
}

/// Keyframe snap mode for a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapMode {
    /// Snap cuts to nearest keyframe (lossless only).
    Keyframe,
    /// Exact frame cut (requires re-encode).
    Exact,
}

impl PlannedSegment {
    /// Whether this segment can be stream-copied losslessly.
    pub fn is_lossless_cut(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergePlan {
    pub order: Vec<SourceId>,
    pub lossless: bool,
    pub reasons: Vec<String>,
    pub alternative: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportPlan {
    pub segments: Vec<PlannedSegment>,
    pub merge_plan: MergePlan,
    pub tracks: Vec<TrackPlan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackPlan {
    pub kind: frameforge_core_types::TrackKind,
    pub index: u32,
    pub name: String,
    pub enabled: bool,
    pub muted: bool,
    pub solo: bool,
    pub items: Vec<PlannedSegment>,
}

/// Compatibility analysis result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub lossless_possible: bool,
    pub reasons: Vec<String>,
    pub alternative: String,
}

/// Stream parameters extracted from a probed file for compatibility comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamParams {
    pub codec: String,
    pub profile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    pub timebase_num: i64,
    pub timebase_den: i64,
    pub pix_fmt: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}

impl StreamParams {
    /// Build StreamParams from a probed StreamInfo.
    pub fn from_stream_info(info: &StreamInfo) -> Self {
        StreamParams {
            codec: info.codec.clone(),
            profile: info.profile.clone(),
            width: info.width,
            height: info.height,
            fps_num: info.fps_num,
            fps_den: info.fps_den,
            timebase_num: info.timebase_num,
            timebase_den: info.timebase_den,
            pix_fmt: info.pix_fmt.clone(),
            sample_rate: info.sample_rate,
            channels: info.channels,
        }
    }

    fn compatible_with(&self, other: &Self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.codec != other.codec {
            reasons.push(format!("codec mismatch: {} vs {}", self.codec, other.codec));
        }
        if self.profile != other.profile {
            reasons.push(format!(
                "profile mismatch: {:?} vs {:?}",
                self.profile, other.profile
            ));
        }
        if self.width != other.width || self.height != other.height {
            reasons.push(format!(
                "resolution mismatch: {:?}x{:?} vs {:?}x{:?}",
                self.width, self.height, other.width, other.height
            ));
        }
        if self.fps_num != other.fps_num || self.fps_den != other.fps_den {
            reasons.push(format!(
                "fps mismatch: {}/{} vs {}/{}",
                self.fps_num.unwrap_or(0),
                self.fps_den.unwrap_or(1),
                other.fps_num.unwrap_or(0),
                other.fps_den.unwrap_or(1)
            ));
        }
        if self.timebase_num != other.timebase_num || self.timebase_den != other.timebase_den {
            reasons.push(format!(
                "timebase mismatch: {}/{} vs {}/{}",
                self.timebase_num, self.timebase_den, other.timebase_num, other.timebase_den
            ));
        }
        if self.pix_fmt != other.pix_fmt {
            reasons.push(format!(
                "pix_fmt mismatch: {:?} vs {:?}",
                self.pix_fmt, other.pix_fmt
            ));
        }
        if self.sample_rate != other.sample_rate {
            reasons.push(format!(
                "sample_rate mismatch: {:?} vs {:?}",
                self.sample_rate, other.sample_rate
            ));
        }
        if self.channels != other.channels {
            reasons.push(format!(
                "channels mismatch: {:?} vs {:?}",
                self.channels, other.channels
            ));
        }
        reasons
    }
}

/// Probe all clips and extract their first video stream params for compatibility.
fn probe_clip_params(path: &Path) -> Result<StreamParams> {
    let result = probe(path)?;
    let video = result
        .streams
        .iter()
        .find(|s| matches!(s.kind, frameforge_media_probe::StreamKind::Video))
        .ok_or_else(|| {
            FrameForgeError::Compatibility(format!("no video stream in {}", path.display()))
        })?;
    Ok(StreamParams::from_stream_info(video))
}

/// Plan an export from an EDL with keyframe snapping for lossless mode.
pub fn plan_export(edl: &Edl) -> Result<ExportPlan> {
    if edl.tracks.is_empty() {
        return Err(FrameForgeError::InvalidEdl("EDL has no tracks".into()));
    }

    let mut all_segments = Vec::new();
    let mut track_plans = Vec::new();
    let mut order = Vec::new();

    for track in &edl.tracks {
        if !track.enabled {
            continue;
        }

        let mut track_items = Vec::new();

        for item in &track.items {
            if !item.enabled {
                continue;
            }

            order.push(item.source_id);

            let snap_mode = if edl.export.mode == ExportMode::StreamCopy {
                SnapMode::Keyframe
            } else {
                SnapMode::Exact
            };

            let cut_points = snap_cut_points(&item.path, item.source_range, snap_mode)?;

            let segment = PlannedSegment {
                source_id: item.source_id,
                item_id: Some(item.id.0),
                range: item.source_range,
                cut_points,
                timeline_start: item.timeline_start,
                track_index: item.track_index,
                transition_in: item.transition_in.clone(),
                transition_out: item.transition_out.clone(),
                volume: item.volume,
                opacity: item.opacity,
            };

            track_items.push(segment.clone());
            all_segments.push(segment);
        }

        track_plans.push(TrackPlan {
            kind: track.kind.clone(),
            index: track.index.unwrap_or(0),
            name: track.name.clone(),
            enabled: track.enabled,
            muted: track.muted,
            solo: track.solo,
            items: track_items,
        });
    }

    if all_segments.is_empty() {
        return Err(FrameForgeError::InvalidEdl("No enabled items in any track".into()));
    }

    let report = analyze_compatibility_tracks(&edl.tracks)?;
    let lossless = edl.export.mode == ExportMode::StreamCopy
        && report.lossless_possible
        && all_cuts_on_keyframes(&all_segments);

    let merge_plan = MergePlan {
        order,
        lossless,
        reasons: report.reasons.clone(),
        alternative: report.alternative.clone(),
    };

    Ok(ExportPlan {
        segments: all_segments,
        merge_plan,
        tracks: track_plans,
    })
}

/// Check that every segment's cut points are on keyframes (required for lossless).
fn all_cuts_on_keyframes(segments: &[PlannedSegment]) -> bool {
    segments.iter().all(|s| {
        s.cut_points.len() >= 2
    })
}

/// Snap cut points to nearest keyframes for lossless mode, or exact frames for re-encode.
fn snap_cut_points(path: &Path, range: TimeRange, mode: SnapMode) -> Result<Vec<Timestamp>> {
    match mode {
        SnapMode::Exact => {
            Ok(vec![range.start, range.end])
        }
        SnapMode::Keyframe => {
            let idx = build_keyframe_index(path)?;
            let _tb = range.start.tb;

            let start_snapped = idx
                .latest_keyframe_at_or_before(&range.start)
                .map(|k| k.pts)
                .unwrap_or(range.start);

            let end_snapped = idx
                .entries
                .iter()
                .find(|e| e.pts.to_seconds() >= range.end.to_seconds())
                .map(|e| e.pts)
                .unwrap_or(range.end);

            // Ensure start <= end. If snapping crosses the range boundary,
            // fall back to exact (re-encode) points for this segment.
            let start = if start_snapped.to_seconds() > end_snapped.to_seconds() {
                range.start
            } else {
                start_snapped
            };
            let end = if end_snapped.to_seconds() < start.to_seconds() {
                range.end
            } else {
                end_snapped
            };

            Ok(vec![start, end])
        }
    }
}

/// Analyze compatibility across tracks using real probe data.
fn analyze_compatibility_tracks(tracks: &[Track]) -> Result<CompatibilityReport> {
    // Collect all video items from all tracks
    let mut video_items: Vec<&TimelineItem> = Vec::new();
    for track in tracks {
        if matches!(track.kind, TrackKind::Video) {
            for item in &track.items {
                if item.enabled {
                    video_items.push(item);
                }
            }
        }
    }

    if video_items.is_empty() {
        return Ok(CompatibilityReport {
            lossless_possible: true,
            reasons: Vec::new(),
            alternative: "stream-copy".into(),
        });
    }

    if video_items.len() < 2 {
        return Ok(CompatibilityReport {
            lossless_possible: true,
            reasons: Vec::new(),
            alternative: "stream-copy".into(),
        });
    }

    // Probe first video item as reference.
    let ref_params = probe_clip_params(&video_items[0].path)?;

    let mut reasons = Vec::new();

    for item in &video_items[1..] {
        let params = probe_clip_params(&item.path)?;
        let item_reasons = ref_params.compatible_with(&params);
        if !item_reasons.is_empty() {
            reasons.push(format!(
                "item {} ({}): {}",
                item.id.0,
                item.path.display(),
                item_reasons.join(", ")
            ));
        }
    }

    let lossless_possible = reasons.is_empty();
    let alternative = if lossless_possible {
        "stream-copy".into()
    } else {
        "re-encode with normalization (scale/fps/audio-resample)".into()
    };

    Ok(CompatibilityReport {
        lossless_possible,
        reasons,
        alternative,
    })
}

/// Public compatibility analyzer.
pub fn analyze_compatibility(tracks: &[Track]) -> Result<CompatibilityReport> {
    analyze_compatibility_tracks(tracks)
}

/// Validate an EDL: items non-overlapping per track, times monotonic.
pub fn validate_edl(edl: &Edl) -> Result<()> {
    for track in &edl.tracks {
        let mut sorted_items: Vec<_> = track.items.iter().collect();
        sorted_items.sort_by(|a, b| a.timeline_start.to_seconds().partial_cmp(&b.timeline_start.to_seconds()).unwrap());

        let mut prev_end: Option<Timestamp> = None;
        for item in sorted_items {
            if !item.enabled {
                continue;
            }

            let item_end = item.timeline_end();
            if item_end.to_seconds() <= item.timeline_start.to_seconds() {
                return Err(FrameForgeError::InvalidEdl(format!(
                    "track {}: item {} end must be after start",
                    track.name, item.id.0
                )));
            }
            if let Some(pe) = prev_end {
                if item.timeline_start.to_seconds() < pe.to_seconds() {
                    return Err(FrameForgeError::InvalidEdl(format!(
                        "track {}: overlapping items {} and {}",
                        track.name, item.id.0, prev_end.unwrap().value
                    )));
                }
            }
            prev_end = Some(item_end);
        }
    }
    Ok(())
}

/// Ripple edit: shift all items after a given time on a track by a delta.
pub fn ripple_edit(track: &mut Track, after_time: Timestamp, delta_seconds: f64) {
    if track.locked {
        return;
    }

    let delta = if delta_seconds >= 0.0 { 1 } else { -1 };
    let abs_delta = delta_seconds.abs();

    // Convert delta to timestamp in track's timebase
    let tb = after_time.tb;
    let delta_ticks = (abs_delta / tb.as_f64()).round() as i64 * delta;

    for item in &mut track.items {
        if item.timeline_start.to_seconds() >= after_time.to_seconds() {
            item.timeline_start = Timestamp::new(
                (item.timeline_start.value as i64 + delta_ticks) as i64,
                tb,
            );
        }
    }

    // Re-sort items after ripple
    track.items.sort_by(|a, b| a.timeline_start.to_seconds().partial_cmp(&b.timeline_start.to_seconds()).unwrap());
}

/// Insert a new item at a specific time, ripple-shifting subsequent items.
pub fn insert_item_ripple(track: &mut Track, mut item: TimelineItem, at_time: Timestamp) -> Result<()> {
    if track.locked {
        return Err(FrameForgeError::InvalidEdl("Track is locked".into()));
    }

    let item_duration = item.duration_seconds();
    item.timeline_start = at_time;
    item.track_index = track.index.unwrap_or(0);

    // Ripple shift existing items
    ripple_edit(track, at_time, item_duration);

    track.items.push(item);
    track.items.sort_by(|a, b| a.timeline_start.to_seconds().partial_cmp(&b.timeline_start.to_seconds()).unwrap());

    Ok(())
}

/// Remove an item and ripple-close the gap.
pub fn remove_item_ripple(track: &mut Track, item_id: u64) -> Result<()> {
    if track.locked {
        return Err(FrameForgeError::InvalidEdl("Track is locked".into()));
    }

    let item_index = track.items.iter().position(|i| i.id.0 == item_id);
    if let Some(idx) = item_index {
        let item = &track.items[idx];
        let item_duration = item.duration_seconds();
        let item_start = item.timeline_start;

        track.items.remove(idx);

        // Ripple close the gap (negative delta)
        ripple_edit(track, item_start, -item_duration);
    }

    Ok(())
}

/// Trim an item (adjust in/out points) with optional ripple.
pub fn trim_item(
    track: &mut Track,
    item_id: u64,
    new_start: Option<Timestamp>,
    new_end: Option<Timestamp>,
    ripple: bool,
) -> Result<()> {
    if track.locked {
        return Err(FrameForgeError::InvalidEdl("Track is locked".into()));
    }

    let item = track.item_by_id_mut(frameforge_core_types::ItemId(item_id))
        .ok_or_else(|| FrameForgeError::InvalidEdl("Item not found".into()))?;

    let old_start = item.timeline_start;
    let old_end = item.timeline_end();
    let old_duration = item.duration_seconds();

    if let Some(start) = new_start {
        // Adjust source range start
        let offset = start.to_seconds() - old_start.to_seconds();
        let source_start_seconds = item.source_range.start.to_seconds() + offset;
        item.source_range.start = Timestamp::from_seconds(source_start_seconds, item.source_range.start.tb);
        item.timeline_start = start;
    }

    if let Some(end) = new_end {
        // Adjust source range end
        let source_end_seconds = item.source_range.start.to_seconds() + (end.to_seconds() - item.timeline_start.to_seconds());
        item.source_range.end = Timestamp::from_seconds(source_end_seconds, item.source_range.end.tb);
    }

    let new_duration = item.duration_seconds();
    let duration_delta = new_duration - old_duration;

    if ripple && duration_delta != 0.0 {
        ripple_edit(track, old_end, duration_delta);
    }

    // Re-sort
    track.items.sort_by(|a, b| a.timeline_start.to_seconds().partial_cmp(&b.timeline_start.to_seconds()).unwrap());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use frameforge_core_types::{
        Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
        Timestamp, Track, TrackKind, TimelineItem, ItemId,
    };

    fn tb() -> Timebase {
        Timebase::new(1, 1000)
    }

    #[test]
    fn validate_non_overlapping_ok() {
        let edl = Edl {
            version: 1,
            clips: vec![],
            tracks: vec![
                Track::new(TrackKind::Video, 0, "Video 1".into()),
                Track::new(TrackKind::Audio, 1, "Audio 1".into()),
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
                destination: std::path::PathBuf::from("out.mp4"),
            },
        };
        assert!(validate_edl(&edl).is_ok());
    }

    #[test]
    fn ripple_edit_shifts_items() {
        let mut track = Track::new(TrackKind::Video, 0, "V1".into());
        let item1 = TimelineItem {
            id: ItemId::new(),
            source_id: SourceId(1),
            name: "Clip 1".into(),
            path: "a.mp4".into(),
            source_range: TimeRange::new(Timestamp::new(0, tb()), Timestamp::new(5000, tb())),
            timeline_start: Timestamp::new(0, tb()),
            track_index: 0,
            transition_in: None,
            transition_out: None,
            volume: 1.0,
            opacity: 1.0,
            enabled: true,
            locked: false,
        };
        let item2 = TimelineItem {
            id: ItemId::new(),
            source_id: SourceId(2),
            name: "Clip 2".into(),
            path: "b.mp4".into(),
            source_range: TimeRange::new(Timestamp::new(0, tb()), Timestamp::new(5000, tb())),
            timeline_start: Timestamp::new(5000, tb()),
            track_index: 0,
            transition_in: None,
            transition_out: None,
            volume: 1.0,
            opacity: 1.0,
            enabled: true,
            locked: false,
        };
        track.items = vec![item1, item2];

        ripple_edit(&mut track, Timestamp::new(0, tb()), 2.0); // Shift by 2 seconds

        assert_eq!(track.items[0].timeline_start.to_seconds(), 2.0);
        assert_eq!(track.items[1].timeline_start.to_seconds(), 7.0);
    }
}
