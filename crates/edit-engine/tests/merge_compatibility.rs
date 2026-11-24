//! Merge-compatibility matrix tests for FrameForge.
//!
//! These tests verify that the compatibility analyzer correctly identifies
//! when clips can be losslessly merged vs. when re-encoding is required.

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, ItemId, SourceId, TimeRange, Timebase,
    TimelineItem, Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::{analyze_compatibility, plan_export, validate_edl};
use std::path::PathBuf;

fn tb() -> Timebase {
    Timebase::new(1, 1000)
}

fn make_clip(id: u64, path: &str, segments: Vec<(i64, i64)>) -> Clip {
    let clip_segments: Vec<frameforge_core_types::Segment> = segments
        .iter()
        .map(|(start_ms, end_ms)| {
            let start = Timestamp::new(*start_ms, tb());
            let end = Timestamp::new(*end_ms, tb());
            frameforge_core_types::Segment {
                range: TimeRange::new(start, end),
                source_id: SourceId(id),
            }
        })
        .collect();

    Clip {
        id: SourceId(id),
        path: PathBuf::from(path),
        segments: clip_segments,
    }
}

fn make_edl(clips: Vec<Clip>, mode: ExportMode) -> Edl {
    // Derive video-track timeline items from clip segments (mirrors project app).
    let mut video_items = Vec::new();
    let mut cursor = 0.0f64;
    for clip in &clips {
        for seg in &clip.segments {
            let dur = seg.range.duration_seconds();
            video_items.push(TimelineItem {
                id: ItemId::default(),
                source_id: clip.id,
                name: "clip".into(),
                path: clip.path.clone(),
                source_range: seg.range,
                timeline_start: Timestamp::new((cursor * 1000.0) as i64, tb()),
                track_index: 0,
                transition_in: None,
                transition_out: None,
                volume: 1.0,
                opacity: 1.0,
                enabled: true,
                locked: false,
            });
            cursor += dur;
        }
    }

    Edl {
        version: 1,
        clips,
        tracks: vec![
            Track {
                kind: TrackKind::Video,
                enabled: true,
                index: Some(0),
                name: "Video".into(),
                height: 80.0,
                muted: false,
                solo: false,
                locked: false,
                items: video_items,
            },
            Track {
                kind: TrackKind::Audio,
                enabled: true,
                index: Some(1),
                name: "Audio".into(),
                height: 60.0,
                muted: false,
                solo: false,
                locked: false,
                items: Vec::new(),
            },
        ],
        export: ExportSettings {
            mode,
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

/// Test: validate_edl rejects overlapping segments
#[test]
fn merge_overlapping_segments_rejected() {
    let clip1 = make_clip(1, "test-a.mp4", vec![(0, 5000), (4000, 8000)]);

    let mut edl = make_edl(vec![clip1], ExportMode::StreamCopy);
    // Force timeline overlap so validate_edl rejects it.
    if let Some(track) = edl
        .tracks
        .iter_mut()
        .find(|t| matches!(t.kind, TrackKind::Video))
    {
        for item in &mut track.items {
            item.timeline_start = Timestamp::new(0, tb());
        }
    }

    assert!(validate_edl(&edl).is_err());
}

/// Test: validate_edl accepts non-overlapping segments
#[test]
fn merge_non_overlapping_segments_accepted() {
    let clip1 = make_clip(1, "test-a.mp4", vec![(0, 5000), (6000, 10000)]);

    let edl = make_edl(vec![clip1], ExportMode::StreamCopy);

    assert!(validate_edl(&edl).is_ok());
}

/// Test: empty EDL is accepted (no clips to validate)
#[test]
fn merge_empty_edl_accepted() {
    let edl = make_edl(vec![], ExportMode::StreamCopy);

    assert!(validate_edl(&edl).is_ok());
}

/// Test: segment with end <= start is rejected
#[test]
fn merge_invalid_segment_rejected() {
    let clip1 = make_clip(1, "test-a.mp4", vec![(5000, 5000)]);

    let edl = make_edl(vec![clip1], ExportMode::StreamCopy);

    assert!(validate_edl(&edl).is_err());
}

/// Test: clip with end before start is rejected
#[test]
fn merge_reversed_segment_rejected() {
    let clip1 = make_clip(1, "test-a.mp4", vec![(5000, 1000)]);

    let edl = make_edl(vec![clip1], ExportMode::StreamCopy);

    assert!(validate_edl(&edl).is_err());
}

fn make_video_track(items: Vec<TimelineItem>) -> Track {
    Track {
        kind: TrackKind::Video,
        enabled: true,
        index: Some(0),
        name: "Video".into(),
        height: 80.0,
        muted: false,
        solo: false,
        locked: false,
        items,
    }
}

fn make_timeline_item(id: u64, path: &str) -> TimelineItem {
    TimelineItem {
        id: ItemId(id),
        source_id: SourceId(id),
        name: format!("clip-{}", id),
        path: std::path::PathBuf::from(path),
        source_range: TimeRange::new(Timestamp::new(0, tb()), Timestamp::new(5000, tb())),
        timeline_start: Timestamp::new(0, tb()),
        track_index: 0,
        transition_in: None,
        transition_out: None,
        volume: 1.0,
        opacity: 1.0,
        enabled: true,
        locked: false,
    }
}

/// Test: two clips with fake paths - probe will fail, returning error
#[test]
fn compatibility_two_clips_fake_paths_error() {
    let track = make_video_track(vec![
        make_timeline_item(1, "test-a.mp4"),
        make_timeline_item(2, "test-b.mp4"),
    ]);

    // Probe will fail on fake paths, so analyze_compatibility returns Err
    let result = analyze_compatibility(&[track]);
    assert!(result.is_err());
}

/// Test: no clips is lossless (edge case)
#[test]
fn compatibility_no_clips_lossless() {
    let track = make_video_track(vec![]);

    let report = analyze_compatibility(&[track]).expect("analyze should succeed");

    assert!(report.lossless_possible);
    assert!(report.reasons.is_empty());
}

/// Test: plan_export with StreamCopy mode - fake path returns error (probe fails)
#[test]
fn plan_export_stream_copy_mode() {
    let clip1 = make_clip(1, "test-a.mp4", vec![(0, 5000)]);

    let edl = make_edl(vec![clip1], ExportMode::StreamCopy);

    let result = plan_export(&edl);
    assert!(result.is_err());
}

/// Test: plan_export with Reencode mode - single clip returns Ok (Exact mode doesn't probe)
#[test]
fn plan_export_reencode_mode() {
    let clip1 = make_clip(1, "test-a.mp4", vec![(0, 5000)]);

    let edl = make_edl(vec![clip1], ExportMode::Reencode);

    let plan = plan_export(&edl).expect("plan_export should succeed");
    assert_eq!(plan.merge_plan.order.len(), 1);
    assert_eq!(plan.segments.len(), 1);
}
