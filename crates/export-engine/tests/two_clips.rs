use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, ItemId, Segment, SourceId, TimeRange,
    Timebase, TimelineItem, Timestamp, Track, TrackKind,
};
use frameforge_export_engine::{ExportExecutor, StreamCopyExecutor};
use std::process::Command;

fn tb() -> Timebase {
    Timebase::new(1, 1_000_000)
}
fn ts(s: f64) -> Timestamp {
    Timestamp::from_seconds(s, tb())
}

fn gen(path: &std::path::Path) {
    let st = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=4:size=320x240:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:duration=4",
            "-shortest",
            "-c:v",
            "libx264",
            "-g",
            "30",
            "-bf",
            "0",
            "-c:a",
            "aac",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
}

#[test]
fn two_clips_export() {
    let dir = std::env::temp_dir();
    let a = dir.join(format!("a_{}.mp4", std::process::id()));
    let b = dir.join(format!("b_{}.mp4", std::process::id()));
    let out = dir.join(format!("out_{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&out);
    gen(&a);
    gen(&b);

    let edl = Edl {
        version: 1,
        clips: vec![
            Clip {
                id: SourceId(1),
                path: a.clone(),
                segments: vec![Segment {
                    range: TimeRange::new(ts(0.0), ts(4.0)),
                    source_id: SourceId(1),
                }],
            },
            Clip {
                id: SourceId(2),
                path: b.clone(),
                segments: vec![Segment {
                    range: TimeRange::new(ts(0.0), ts(4.0)),
                    source_id: SourceId(2),
                }],
            },
        ],
        tracks: vec![Track {
            kind: TrackKind::Video,
            enabled: true,
            index: Some(0),
            name: "V".into(),
            height: 80.0,
            muted: false,
            solo: false,
            locked: false,
            items: vec![
                TimelineItem {
                    id: ItemId::default(),
                    source_id: SourceId(1),
                    name: "a".into(),
                    path: a.clone(),
                    source_range: TimeRange::new(ts(0.0), ts(4.0)),
                    timeline_start: ts(0.0),
                    track_index: 0,
                    transition_in: None,
                    transition_out: None,
                    volume: 1.0,
                    opacity: 1.0,
                    enabled: true,
                    locked: false,
                },
                TimelineItem {
                    id: ItemId::default(),
                    source_id: SourceId(2),
                    name: "b".into(),
                    path: b.clone(),
                    source_range: TimeRange::new(ts(0.0), ts(4.0)),
                    timeline_start: ts(4.0),
                    track_index: 0,
                    transition_in: None,
                    transition_out: None,
                    volume: 1.0,
                    opacity: 1.0,
                    enabled: true,
                    locked: false,
                },
            ],
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
            destination: out.clone(),
        },
    };

    frameforge_edit_engine::validate_edl(&edl).unwrap();
    let plan = frameforge_edit_engine::plan_export(&edl).unwrap();
    let mut exec = StreamCopyExecutor::new(edl.clips.clone(), out.clone());
    let cancel = frameforge_export_engine::CancelToken::new();
    exec.run(&plan, &|_| {}, &cancel)
        .expect("export should succeed");
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&out);
}
