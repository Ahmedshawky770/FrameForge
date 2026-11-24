//! Phase 6: UI-facing export metadata (path label, reason, badge, estimate).
//!
//! The HTML layer is not modified; these tests validate the Rust data that the
//! UI consumes.

mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::plan_export;
use frameforge_export_engine::{
    estimate_export_seconds, export_reason, lossless_badge_visible, ExportPath,
};
use std::path::{Path, PathBuf};

fn tb() -> Timebase {
    Timebase::new(1, 1_000_000)
}
fn ts(s: f64) -> Timestamp {
    Timestamp::from_seconds(s, tb())
}

fn make_edl(path: &Path, start: f64, end: f64, mode: ExportMode) -> Edl {
    Edl {
        version: 1,
        clips: vec![Clip {
            id: SourceId(1),
            path: path.to_path_buf(),
            segments: vec![Segment {
                range: TimeRange::new(ts(start), ts(end)),
                source_id: SourceId(1),
            }],
        }],
        tracks: vec![Track {
            kind: TrackKind::Video,
            enabled: true,
            index: Some(0),
            name: "V".into(),
            height: 80.0,
            muted: false,
            solo: false,
            locked: false,
            items: vec![],
        }],
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
            destination: PathBuf::new(),
        },
    }
}

fn plan_for(
    path: &Path,
    start: f64,
    end: f64,
    mode: ExportMode,
) -> frameforge_edit_engine::ExportPlan {
    plan_export(&make_edl(path, start, end, mode)).expect("plan_export")
}

#[test]
fn phase6_export_path_label() {
    assert_eq!(
        ExportPath::CopyLossless.label(),
        "copy (lossless stream copy)"
    );
    assert_eq!(
        ExportPath::SmartCopy.label(),
        "smart copy (copy GOPs + boundary re-encode)"
    );
    assert_eq!(ExportPath::FullReencode.label(), "full re-encode");
}

#[test]
fn phase6_export_reason() {
    let dir = std::env::temp_dir().join(format!("ff_p6_reason_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let mid = plan_for(&src, 0.0, 2.5, ExportMode::StreamCopy);
    let reason = export_reason(&mid, ExportPath::SmartCopy);
    eprintln!("P6 smart reason: {}", reason);
    assert!(reason.contains("boundary"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase6_lossless_badge_only_for_lossless() {
    let dir = std::env::temp_dir().join(format!("ff_p6_badge_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let mid = plan_for(&src, 0.0, 2.5, ExportMode::StreamCopy);
    let full = plan_for(&src, 0.0, 4.0, ExportMode::StreamCopy);
    eprintln!(
        "P6 badge: mid lossless={} full lossless={}",
        mid.merge_plan.lossless, full.merge_plan.lossless
    );
    assert!(
        !lossless_badge_visible(&mid),
        "mid-GOP must not show Lossless OK"
    );
    assert!(
        lossless_badge_visible(&full),
        "full-file copy may show Lossless OK"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase6_estimated_time() {
    let dir = std::env::temp_dir().join(format!("ff_p6_est_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let mid = plan_for(&src, 0.0, 2.5, ExportMode::StreamCopy);
    let smart = estimate_export_seconds(&mid, ExportPath::SmartCopy, false);
    let full = estimate_export_seconds(&mid, ExportPath::FullReencode, false);
    let copy = estimate_export_seconds(&mid, ExportPath::CopyLossless, false);
    eprintln!("P6 estimates: copy={} smart={} full={}", copy, smart, full);
    assert!(copy < smart);
    assert!(smart < full);
    let _ = std::fs::remove_dir_all(&dir);
}
