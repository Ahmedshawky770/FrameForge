//! Phase 9: final acceptance criteria.
//!
//! End-to-end checks across MP4 re-encode, MKV smart copy, UI metadata, and
//! operational safety.  libav only; mpv playback is optional.

mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::plan_export;
use frameforge_export_engine::{
    disk_space_free, ensure_disk_space, estimate_export_seconds, export_reason,
    lossless_badge_visible, CancelToken, ExportExecutor, ExportPath, SmartRenderExecutor,
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

fn video_audio_durations(path: &Path) -> (Option<f64>, Option<f64>) {
    let ictx = ffmpeg_next::format::input(path).expect("open");
    let mut v = None;
    let mut a = None;
    for stream in ictx.streams() {
        let d = stream.duration();
        let dur = if d == i64::MIN || d <= 0 {
            None
        } else {
            let tb = stream.time_base();
            Some(d as f64 * tb.0 as f64 / tb.1 as f64)
        };
        match stream.parameters().medium() {
            ffmpeg_next::media::Type::Video => v = dur,
            ffmpeg_next::media::Type::Audio => a = dur,
            _ => {}
        }
    }
    (v, a)
}

fn video_sizes(path: &Path) -> Vec<usize> {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    let mut out = Vec::new();
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            out.push(pkt.size());
        }
    }
    out
}

fn video_packets(path: &Path) -> Vec<Vec<u8>> {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    let mut out = Vec::new();
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            out.push(pkt.data().unwrap_or(&[]).to_vec());
        }
    }
    out
}

fn decode_errors(path: &Path) -> usize {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    let stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .expect("video stream")
        .parameters();
    let mut dec_ctx =
        ffmpeg_next::codec::Context::from_parameters(stream.clone()).expect("decoder");
    let mut dec = dec_ctx.decoder().video().expect("decoder video");
    dec.set_parameters(stream).ok();
    let mut errors = 0;
    let mut frame = ffmpeg_next::util::frame::Video::empty();
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if let Err(e) = dec.send_packet(&pkt) {
            if !matches!(e, ffmpeg_next::Error::Eof) {
                errors += 1;
            }
        }
        while dec.receive_frame(&mut frame).is_ok() {}
    }
    let _ = dec.send_eof();
    while dec.receive_frame(&mut frame).is_ok() {}
    errors
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
fn phase9_mp4_reencode_end_to_end() {
    let dir = std::env::temp_dir().join(format!("ff_p9_mp4_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 180, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 4.0, ExportMode::Reencode);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into())
            .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let (v, a) = video_audio_durations(&out);
    eprintln!("P9 MP4: video={:?} audio={:?}", v, a);
    assert!((v.unwrap() - 4.0).abs() < 0.25);
    assert!((a.unwrap() - 4.0).abs() < 0.35);
    assert_eq!(decode_errors(&out), 0);
    assert!(leftovers(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase9_mkv_smart_copy_end_to_end() {
    let dir = std::env::temp_dir().join(format!("ff_p9_mkv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video_fast(&src, 180, 30).unwrap();
    let out = dir.join("out.mkv");
    let edl = make_edl(&src, 0.0, 2.5, ExportMode::StreamCopy);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into());
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let sizes = video_sizes(&out);
    eprintln!(
        "P9 MKV smart: packets={} non-zero={}",
        sizes.len(),
        sizes.iter().filter(|s| **s > 0).count()
    );
    assert!(sizes.len() >= 70, "expected copied+reencoded frames");
    assert!(sizes.iter().all(|s| *s > 0), "no 0-byte samples");
    assert_eq!(decode_errors(&out), 0);
    let src_packets = video_packets(&src);
    let out_packets = video_packets(&out);
    let cmp = 30.min(src_packets.len()).min(out_packets.len());
    let identical = (0..cmp)
        .filter(|i| src_packets[*i] == out_packets[*i])
        .count();
    eprintln!("P9 MKV bit-identical first GOP: {}/{}", identical, cmp);
    assert!(identical >= 25, "copied GOP must be bit-identical");
    assert!(leftovers(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase9_ui_and_safety_metadata() {
    let dir = std::env::temp_dir().join(format!("ff_p9_meta_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let mid = plan_for(&src, 0.0, 2.5, ExportMode::StreamCopy);
    assert_eq!(
        ExportPath::SmartCopy.label(),
        "smart copy (copy GOPs + boundary re-encode)"
    );
    assert!(export_reason(&mid, ExportPath::SmartCopy).contains("boundary"));
    assert!(!lossless_badge_visible(&mid));
    assert!(estimate_export_seconds(&mid, ExportPath::SmartCopy, false) > 0);

    let dest = dir.join("out.mp4");
    let free = disk_space_free(&dest).unwrap();
    let err = ensure_disk_space(&dest, free + 1).expect_err("must fail");
    eprintln!("P9 disk precheck: {err}");
    assert!(err.to_string().contains("disk space"));
    assert!(!dest.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase9_playback_mpv() {
    let dir = std::env::temp_dir().join(format!("ff_p9_mpv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 120, 50, 0.0).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 1.0, ExportMode::Reencode);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into())
            .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    match std::process::Command::new("mpv")
        .args(["--no-config", "--really-quiet", "--frames=1"])
        .arg(&out)
        .status()
    {
        Ok(status) => {
            eprintln!("P9 mpv status: {:?}", status);
            assert!(status.success());
        }
        Err(e) => eprintln!("P9 mpv not available, skipping: {e}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
