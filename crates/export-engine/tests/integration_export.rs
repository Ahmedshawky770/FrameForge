use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, ItemId, Segment, SourceId, TimeRange,
    Timebase, TimelineItem, Timestamp, Track, TrackKind,
};
use frameforge_export_engine::{ExportExecutor, StreamCopyExecutor};
use std::process::Command;

fn tb() -> Timebase {
    Timebase::new(1, 1_000_000)
}
fn seconds_to_timestamp(seconds: f64) -> Timestamp {
    Timestamp::from_seconds(seconds, tb())
}

fn generate_clip(path: &std::path::Path) {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=10:size=320x240:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:duration=10",
            "-shortest",
            "-c:v",
            "libx264",
            "-g",
            "30",
            "-keyint_min",
            "30",
            "-sc_threshold",
            "0",
            "-bf",
            "0",
            "-c:a",
            "aac",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg must be available");
    assert!(status.success(), "ffmpeg failed to generate clip");
}

#[test]
fn stream_copy_delete_middle_preserves_timing() {
    let dir = std::env::temp_dir();
    let input = dir.join(format!("ff_test_input_{}.mp4", std::process::id()));
    let output = dir.join(format!("ff_test_output_{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);

    generate_clip(&input);

    let segments = vec![(0.0f64, 3.0f64), (6.0f64, 10.0f64)];
    let mut clip_segments = Vec::new();
    for (s, e) in &segments {
        clip_segments.push(Segment {
            range: TimeRange::new(seconds_to_timestamp(*s), seconds_to_timestamp(*e)),
            source_id: SourceId(1),
        });
    }
    let mut video_items = Vec::new();
    let mut cursor = 0.0f64;
    for (s, e) in &segments {
        video_items.push(TimelineItem {
            id: ItemId::default(),
            source_id: SourceId(1),
            name: "clip".into(),
            path: input.clone(),
            source_range: TimeRange::new(seconds_to_timestamp(*s), seconds_to_timestamp(*e)),
            timeline_start: seconds_to_timestamp(cursor),
            track_index: 0,
            transition_in: None,
            transition_out: None,
            volume: 1.0,
            opacity: 1.0,
            enabled: true,
            locked: false,
        });
        cursor += e - s;
    }

    let edl = Edl {
        version: 1,
        clips: vec![Clip {
            id: SourceId(1),
            path: input.clone(),
            segments: clip_segments,
        }],
        tracks: vec![
            Track {
                kind: TrackKind::Video,
                enabled: true,
                index: Some(0),
                name: "Video 1".into(),
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
                name: "Audio 1".into(),
                height: 60.0,
                muted: false,
                solo: false,
                locked: false,
                items: Vec::new(),
            },
        ],
        export: frameforge_core_types::ExportSettings {
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
            destination: output.clone(),
        },
    };

    frameforge_edit_engine::validate_edl(&edl).expect("validate_edl should pass");
    let plan = frameforge_edit_engine::plan_export(&edl).expect("plan_export should pass");

    let mut executor = StreamCopyExecutor::new(edl.clips.clone(), output.clone());
    let progress = |_p: f32| {};
    let cancel = frameforge_export_engine::CancelToken::new();
    executor
        .run(&plan, &progress, &cancel)
        .expect("export should succeed");

    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=dts_time,pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(&output)
        .output()
        .expect("ffprobe");
    assert!(out.status.success(), "ffprobe failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut dts_vals = Vec::new();
    let mut pts_vals = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() >= 2 {
            if let (Ok(d), Ok(p)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
                dts_vals.push(d);
                pts_vals.push(p);
            }
        }
    }
    assert!(!dts_vals.is_empty(), "no packets");

    let out2 = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&output)
        .output()
        .expect("ffprobe");
    let dur: f64 = String::from_utf8_lossy(&out2.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0);
    assert!((dur - 7.0).abs() < 1.0, "duration should be ~7s, got {dur}");

    assert!(dts_vals[0].abs() < 0.5, "first dts not ~0: {}", dts_vals[0]);
    for w in dts_vals.windows(2) {
        assert!(
            w[1] >= w[0] - 1e-6,
            "dts not monotonic: {} -> {}",
            w[0],
            w[1]
        );
    }

    let mut deltas: Vec<f64> = pts_vals.windows(2).map(|w| w[1] - w[0]).collect();
    deltas.retain(|d| *d > 1e-6);
    if !deltas.is_empty() {
        let avg = deltas.iter().sum::<f64>() / deltas.len() as f64;
        for d in &deltas {
            assert!(
                (d - avg).abs() < 0.05,
                "pts delta not near-constant: {} vs avg {}",
                d,
                avg
            );
        }
    }

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}
