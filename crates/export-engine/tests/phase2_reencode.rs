//! Phase 2: re-encode path (timestamps, audio, hardware report).
//!
//! Test-first: these tests encode the expected behaviour.  They use libav for
//! fixtures and never invoke the `ffmpeg` CLI.

mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::{plan_export, ExportPlan};
use frameforge_export_engine::{
    clear_last_encoder_report, last_encoder_report, CancelToken, ExportExecutor, ReencodeExecutor,
    SmartRenderExecutor,
};
use std::path::{Path, PathBuf};

fn tb() -> Timebase {
    Timebase::new(1, 1_000_000)
}
fn ts(s: f64) -> Timestamp {
    Timestamp::from_seconds(s, tb())
}

fn make_multi_edl(path: &Path, ranges: &[(f64, f64)], mode: ExportMode) -> Edl {
    let clips = ranges
        .iter()
        .enumerate()
        .map(|(i, (start, end))| {
            let id = SourceId((i + 1) as u64);
            Clip {
                id,
                path: path.to_path_buf(),
                segments: vec![Segment {
                    range: TimeRange::new(ts(*start), ts(*end)),
                    source_id: id,
                }],
            }
        })
        .collect();
    Edl {
        version: 1,
        clips,
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

fn make_multi_plan(path: &Path, ranges: &[(f64, f64)], mode: ExportMode) -> (Edl, ExportPlan) {
    let edl = make_multi_edl(path, ranges, mode);
    let plan = plan_export(&edl).expect("plan_export");
    (edl, plan)
}

fn stream_duration_seconds(stream: &ffmpeg_next::Stream) -> Option<f64> {
    let d = stream.duration();
    if d == i64::MIN || d <= 0 {
        return None;
    }
    let tb = stream.time_base();
    Some(d as f64 * tb.0 as f64 / tb.1 as f64)
}

fn video_audio_durations(path: &Path) -> (Option<f64>, Option<f64>) {
    let ictx = ffmpeg_next::format::input(path).expect("open output");
    let mut video = None;
    let mut audio = None;
    for stream in ictx.streams() {
        match stream.parameters().medium() {
            ffmpeg_next::media::Type::Video => video = stream_duration_seconds(&stream),
            ffmpeg_next::media::Type::Audio => audio = stream_duration_seconds(&stream),
            _ => {}
        }
    }
    (video, audio)
}

#[test]
fn phase2_reencode_multi_segment_duration_is_exact() {
    let dir = std::env::temp_dir().join(format!("ff_p2_reenc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 120, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    let (edl, plan) = make_multi_plan(&src, &[(0.0, 1.0), (2.0, 3.0)], ExportMode::Reencode);
    clear_last_encoder_report();
    let mut exec =
        ReencodeExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into());
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let (v, a) = video_audio_durations(&out);
    eprintln!("P2 reencode multi: video={:?} audio={:?}", v, a);
    assert!((v.unwrap() - 2.0).abs() < 0.25, "video duration");
    assert!((a.unwrap() - 2.0).abs() < 0.35, "audio duration");
    let report = last_encoder_report().expect("encoder report");
    eprintln!(
        "P2 reencode report: requested={} encoder={} hardware={} fallback={:?}",
        report.requested, report.encoder, report.hardware, report.fallback_reason
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase2_smart_reencode_multi_segment_duration_is_exact() {
    let dir = std::env::temp_dir().join(format!("ff_p2_smart_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 120, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    let (edl, plan) = make_multi_plan(&src, &[(0.0, 1.0), (2.0, 3.0)], ExportMode::Reencode);
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into())
            .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let (v, a) = video_audio_durations(&out);
    eprintln!("P2 smart reencode multi: video={:?} audio={:?}", v, a);
    assert!((v.unwrap() - 2.0).abs() < 0.25, "video duration");
    assert!((a.unwrap() - 2.0).abs() < 0.35, "audio duration");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase2_audio_video_sync_multi_segment() {
    let dir = std::env::temp_dir().join(format!("ff_p2_sync_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 120, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    let (edl, plan) = make_multi_plan(&src, &[(0.0, 1.0), (2.0, 3.0)], ExportMode::Reencode);
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into())
            .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let (v, a) = video_audio_durations(&out);
    eprintln!("P2 sync: video={:?} audio={:?}", v, a);
    let diff = (v.unwrap() - a.unwrap()).abs();
    assert!(diff < 0.35, "A/V duration diff {diff}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase2_hardware_auto_encoder_report() {
    let dir = std::env::temp_dir().join(format!("ff_p2_hw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 60, 50, 0.0, false).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_multi_edl(&src, &[(0.0, 1.0)], ExportMode::Reencode);
    let plan = plan_export(&edl).expect("plan_export");
    clear_last_encoder_report();
    let mut exec =
        ReencodeExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into());
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let report = last_encoder_report().expect("encoder report");
    eprintln!(
        "P2 auto report: requested={} encoder={} hardware={} fallback={:?}",
        report.requested, report.encoder, report.hardware, report.fallback_reason
    );
    assert_eq!(report.requested, "auto");
    assert!(!report.encoder.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
