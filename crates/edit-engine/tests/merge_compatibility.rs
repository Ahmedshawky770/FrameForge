//! Merge-compatibility matrix tests for FrameForge.
//!
//! These tests verify that the compatibility analyzer correctly identifies
//! when clips can be losslessly merged vs. when re-encoding is required.

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, SourceId, TimeRange, Timebase, Timestamp,
    Track, TrackKind,
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
    Edl {
        version: 1,
        clips,
        tracks: vec![
            Track {
                kind: TrackKind::Video,
                enabled: true,
                index: Some(0),
            },
            Track {
                kind: TrackKind::Audio,
                enabled: true,
                index: Some(1),
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

    let edl = make_edl(vec![clip1], ExportMode::StreamCopy);

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

/// Test: single clip compatibility report - single clip returns Ok without probing
#[test]
fn compatibility_single_clip_ok() {
    let clip1 = make_clip(1, "test-a.mp4", vec![(0, 5000)]);

    let report = analyze_compatibility(&[clip1]).expect("analyze should succeed for single clip");

    assert!(report.lossless_possible);
    assert!(report.reasons.is_empty());
}

/// Test: two clips with fake paths - probe will fail, returning error
#[test]
fn compatibility_two_clips_fake_paths_error() {
    let clip1 = make_clip(1, "test-a.mp4", vec![(0, 5000)]);
    let clip2 = make_clip(2, "test-b.mp4", vec![(0, 5000)]);

    // Probe will fail on fake paths, so analyze_compatibility returns Err
    let result = analyze_compatibility(&[clip1, clip2]);
    assert!(result.is_err());
}

/// Test: no clips is lossless (edge case)
#[test]
fn compatibility_no_clips_lossless() {
    let report = analyze_compatibility(&[]).expect("analyze should succeed");

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
