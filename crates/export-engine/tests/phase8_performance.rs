//! Phase 8: performance and hardware-encoder selection.
//!
//! Test-first, libav only.  The benchmark is relative (copy < smart <=
//! re-encode) and does not assert absolute wall-clock numbers.

mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::plan_export;
use frameforge_export_engine::{
    clear_last_encoder_report, last_encoder_report, CancelToken, ExportExecutor,
    SmartRenderExecutor, StreamCopyExecutor,
};
use std::path::{Path, PathBuf};
use std::time::Instant;

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
fn phase8_hardware_fallback_to_x264() {
    let dir = std::env::temp_dir().join(format!("ff_p8_hw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 60, 50, 0.0, false).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 1.0, ExportMode::Reencode);
    let plan = plan_export(&edl).expect("plan_export");
    clear_last_encoder_report();
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "nvenc".into(), "fast".into())
            .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let report = last_encoder_report().expect("encoder report");
    eprintln!(
        "P8 nvenc request: encoder={} hardware={} fallback={:?}",
        report.encoder, report.hardware, report.fallback_reason
    );
    assert_eq!(report.requested, "nvenc");
    if report.fallback_reason.is_some() {
        assert!(!report.hardware, "fallback must be software");
        assert_eq!(report.encoder, "libx264");
    } else {
        assert!(report.hardware, "NVENC must be hardware when no fallback");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase8_benchmark_copy_vs_reencode_vs_smart() {
    let dir = std::env::temp_dir().join(format!("ff_p8_bench_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video_fast(&src, 180, 30).unwrap();

    // Copy: full stream copy of [0,2.5].
    let copy_out = dir.join("copy.mkv");
    let copy_plan = plan_for(&src, 0.0, 2.5, ExportMode::StreamCopy);
    let t0 = Instant::now();
    let mut copy_exec = StreamCopyExecutor::new(
        make_edl(&src, 0.0, 2.5, ExportMode::StreamCopy).clips,
        copy_out.clone().into(),
    );
    copy_exec
        .run(&copy_plan, &|_| {}, &CancelToken::new())
        .unwrap();
    let copy = t0.elapsed();

    // Full re-encode: [0,2.5] to MP4.
    let re_out = dir.join("re.mp4");
    let re_plan = plan_for(&src, 0.0, 2.5, ExportMode::Reencode);
    let t0 = Instant::now();
    let mut re_exec = SmartRenderExecutor::new(
        make_edl(&src, 0.0, 2.5, ExportMode::Reencode).clips,
        re_out.clone().into(),
        "auto".into(),
        "fast".into(),
    )
    .with_force_reencode(true);
    re_exec.run(&re_plan, &|_| {}, &CancelToken::new()).unwrap();
    let reencode = t0.elapsed();

    // Smart copy: [0,2.5] to MKV (copy [0,2] + re-encode [2,2.5]).
    let smart_out = dir.join("smart.mkv");
    let smart_plan = plan_for(&src, 0.0, 2.5, ExportMode::StreamCopy);
    let t0 = Instant::now();
    let mut smart_exec = SmartRenderExecutor::new(
        make_edl(&src, 0.0, 2.5, ExportMode::StreamCopy).clips,
        smart_out.clone().into(),
        "auto".into(),
        "fast".into(),
    );
    smart_exec
        .run(&smart_plan, &|_| {}, &CancelToken::new())
        .unwrap();
    let smart = t0.elapsed();

    eprintln!(
        "P8 benchmark: copy={:?} smart={:?} reencode={:?}",
        copy, smart, reencode
    );
    assert!(copy < smart, "copy must be faster than smart");
    assert!(
        smart <= reencode.mul_f64(1.5),
        "smart must not be slower than 1.5x full re-encode"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
