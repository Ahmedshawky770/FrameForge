//! Phase 5: audio continuity and A/V sync.
//!
//! Source MP2/AAC -> AAC re-encode with one continuous sample counter.
//! Test-first, libav only.

mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::{plan_export, ExportPlan};
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

fn make_plan(path: &Path, start: f64, end: f64) -> ExportPlan {
    plan_export(&make_edl(path, start, end)).expect("plan_export")
}

fn export_av(src: &Path, out: &Path) {
    let edl = make_edl(src, 0.0, 4.0);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec = SmartRenderExecutor::new(
        edl.clips,
        out.to_path_buf().into(),
        "auto".into(),
        "fast".into(),
    )
    .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
}

fn audio_pts_seconds(path: &Path) -> Vec<f64> {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    let mut pts = Vec::new();
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Audio {
            let tb = stream.time_base();
            let t = pkt.pts().unwrap_or(0) as f64 * tb.0 as f64 / tb.1 as f64;
            pts.push(t);
        }
    }
    pts
}

fn stream_duration(stream: &ffmpeg_next::Stream) -> Option<f64> {
    let d = stream.duration();
    if d == i64::MIN || d <= 0 {
        return None;
    }
    let tb = stream.time_base();
    Some(d as f64 * tb.0 as f64 / tb.1 as f64)
}

fn video_audio_durations(path: &Path) -> (Option<f64>, Option<f64>) {
    let ictx = ffmpeg_next::format::input(path).expect("open");
    let mut v = None;
    let mut a = None;
    for stream in ictx.streams() {
        match stream.parameters().medium() {
            ffmpeg_next::media::Type::Video => v = stream_duration(&stream),
            ffmpeg_next::media::Type::Audio => a = stream_duration(&stream),
            _ => {}
        }
    }
    (v, a)
}

#[test]
fn phase5_audio_continuous_no_gaps() {
    let dir = std::env::temp_dir().join(format!("ff_p5_gaps_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 120, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    export_av(&src, &out);
    let pts = audio_pts_seconds(&out);
    eprintln!("P5 audio packets: {}", pts.len());
    // The final packet is the AAC flush/priming boundary; ignore the last
    // interval and check all mid-stream intervals.
    let mut gaps = 0;
    for i in 1..pts.len().saturating_sub(2) {
        let d = pts[i] - pts[i - 1];
        if d > 0.05 {
            gaps += 1;
            if gaps <= 5 {
                eprintln!("P5 gap at {}: {} -> {}", i, pts[i - 1], pts[i]);
            }
        }
    }
    let max_gap = pts.windows(2).map(|w| w[1] - w[0]).fold(0.0f64, f64::max);
    eprintln!("P5 mid-stream gaps > 50ms: {}, max_gap={}", gaps, max_gap);
    assert_eq!(gaps, 0, "audio must be continuous");
    assert!(max_gap < 0.1, "no gap > 100ms");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase5_audio_duration_matches_video() {
    let dir = std::env::temp_dir().join(format!("ff_p5_dur_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 120, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    export_av(&src, &out);
    let (v, a) = video_audio_durations(&out);
    eprintln!("P5 durations: video={:?} audio={:?}", v, a);
    assert!((v.unwrap() - a.unwrap()).abs() < 0.15, "A/V duration");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase5_audio_video_sync_multi_segment() {
    let dir = std::env::temp_dir().join(format!("ff_p5_sync_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 120, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    // Two segments: [0,1] and [2,3] -> 2s output.
    let edl = Edl {
        version: 1,
        clips: vec![
            Clip {
                id: SourceId(1),
                path: src.clone(),
                segments: vec![Segment {
                    range: TimeRange::new(ts(0.0), ts(1.0)),
                    source_id: SourceId(1),
                }],
            },
            Clip {
                id: SourceId(2),
                path: src.clone(),
                segments: vec![Segment {
                    range: TimeRange::new(ts(2.0), ts(3.0)),
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
    };
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into())
            .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let (v, a) = video_audio_durations(&out);
    eprintln!("P5 multi sync: video={:?} audio={:?}", v, a);
    assert!(
        (v.unwrap() - a.unwrap()).abs() < 0.2,
        "multi-segment A/V sync"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
