mod fixtures;
use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::plan_export;
use frameforge_export_engine::{CancelToken, ExportExecutor, SmartRenderExecutor};
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
            destination: PathBuf::new(),
        },
    }
}
#[test]
fn mp4_high_compat() {
    let dir = std::env::temp_dir().join(format!("ff_mp4_high_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video_profile(&src, 180, 30, "high", "3.0").unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 2.5);
    let plan = plan_export(&edl).expect("plan");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into());
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
