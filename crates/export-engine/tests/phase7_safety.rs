//! Phase 7: operational safety (disk precheck, cancel cleanup, temp cleanup).
//!
//! Test-first, libav only.

mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::plan_export;
use frameforge_export_engine::{
    disk_space_free, ensure_disk_space, CancelToken, ExportExecutor, SmartRenderExecutor,
};
use std::path::{Path, PathBuf};

fn tb() -> Timebase {
    Timebase::new(1, 1_000_000)
}
fn ts(s: f64) -> Timestamp {
    Timestamp::from_seconds(s, tb())
}

fn make_edl(path: &Path, start: f64, end: f64) -> Edl {
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
            mode: ExportMode::Reencode,
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

fn plan_for(path: &Path, start: f64, end: f64) -> frameforge_edit_engine::ExportPlan {
    plan_export(&make_edl(path, start, end)).expect("plan_export")
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect()
}

#[test]
fn phase7_disk_precheck_before_output() {
    let dir = std::env::temp_dir().join(format!("ff_p7_disk_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("out.mp4");
    let free = disk_space_free(&dest).unwrap();
    let err = ensure_disk_space(&dest, free + 1).expect_err("must fail");
    eprintln!("P7 disk precheck: {err}");
    assert!(err.to_string().contains("disk space"));
    assert!(!dest.exists(), "no output may be created");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase7_cancel_smart_reencode_no_temp() {
    let dir = std::env::temp_dir().join(format!("ff_p7_cancel_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 600, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    let plan = plan_for(&src, 0.0, 20.0);
    let cancel = CancelToken::new();
    let cancel2 = cancel.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1));
        cancel2.cancel();
    });
    let mut exec = SmartRenderExecutor::new(
        make_edl(&src, 0.0, 20.0).clips,
        out.clone().into(),
        "auto".into(),
        "fast".into(),
    )
    .with_force_reencode(true);
    let result = exec.run(&plan, &|_| {}, &cancel);
    let _ = handle.join();
    eprintln!(
        "P7 cancel: {:?}, leftovers={:?}",
        result.as_ref().err().map(|e| e.to_string()),
        leftovers(&dir)
    );
    assert!(result.is_err(), "cancelled export must fail");
    assert!(leftovers(&dir).is_empty(), "no .tmp may remain");
    assert!(!out.exists(), "no partial output may remain");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase7_success_cleans_temp() {
    let dir = std::env::temp_dir().join(format!("ff_p7_success_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 60, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 1.0);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into())
            .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    assert!(out.exists());
    assert!(leftovers(&dir).is_empty(), "no .tmp may remain");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase7_failure_cleans_temp() {
    let dir = std::env::temp_dir().join(format!("ff_p7_fail_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 60, 50, 0.0).unwrap();
    let outdir = dir.join("outdir");
    std::fs::create_dir_all(&outdir).unwrap();
    let edl = make_edl(&src, 0.0, 1.0);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec = SmartRenderExecutor::new(
        edl.clips,
        outdir.clone().into(),
        "auto".into(),
        "fast".into(),
    )
    .with_force_reencode(true);
    let result = exec.run(&plan, &|_| {}, &CancelToken::new());
    eprintln!(
        "P7 failure: {:?}, leftovers={:?}",
        result.as_ref().err().map(|e| e.to_string()),
        leftovers(&dir)
    );
    assert!(result.is_err(), "directory destination must fail");
    assert!(leftovers(&dir).is_empty(), "no .tmp may remain");
    let _ = std::fs::remove_dir_all(&dir);
}
