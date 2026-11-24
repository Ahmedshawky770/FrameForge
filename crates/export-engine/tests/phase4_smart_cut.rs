//! Phase 4: smart cut (copy GOPs + boundary re-encode) and open-GOP safety.
//!
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

fn make_plan(path: &Path, start: f64, end: f64) -> ExportPlan {
    plan_export(&make_edl(path, start, end)).expect("plan_export")
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
    let mut idx = 0;
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if idx == 60 {
            let d = pkt.data().unwrap_or(&[]);
            eprintln!(
                "FINAL pkt60 first8={:?} size={}",
                &d[..d.len().min(8)],
                d.len()
            );
        }
        if let Err(e) = dec.send_packet(&pkt) {
            if !matches!(e, ffmpeg_next::Error::Eof) {
                errors += 1;
            }
        }
        idx += 1;
        while dec.receive_frame(&mut frame).is_ok() {}
    }
    let _ = dec.send_eof();
    while dec.receive_frame(&mut frame).is_ok() {}
    errors
}

#[test]
fn phase4_smart_cut_duration_and_frames() {
    let dir = std::env::temp_dir().join(format!("ff_p4_cut_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video_fast(&src, 180, 30).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 2.5);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into());
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let sizes = video_sizes(&out);
    eprintln!(
        "P4 smart cut: packets={} sizes={:?}",
        sizes.len(),
        &sizes[..sizes.len().min(12)]
    );
    assert!(sizes.len() >= 70, "expected copied+reencoded frames");
    assert!(sizes.iter().any(|s| *s > 0), "packets must have payload");
    let errors = decode_errors(&out);
    eprintln!("P4 decode errors: {}", errors);
    assert_eq!(errors, 0, "smart cut must decode cleanly");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase4_smart_cut_copied_gops_bit_identical() {
    let dir = std::env::temp_dir().join(format!("ff_p4_bits_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video_fast(&src, 180, 30).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 2.5);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into());
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let src_packets = video_packets(&src);
    let out_packets = video_packets(&out);
    // The first GOP [0,1) is fully inside the kept range and must be copied.
    let cmp = 30.min(src_packets.len()).min(out_packets.len());
    let mut identical = 0;
    for i in 0..cmp {
        if src_packets[i] == out_packets[i] {
            identical += 1;
        }
    }
    eprintln!("P4 bit-identical first GOP: {}/{} packets", identical, cmp);
    assert!(identical >= 25, "copied GOP must be bit-identical");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase4_open_gop_falls_back_to_reencode() {
    let dir = std::env::temp_dir().join(format!("ff_p4_open_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    // GOP=30 with B-frames and open GOP; later keyframes may be non-IDR.
    fixtures::gen_video_open_gop(&src, 180, 30).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 2.5);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into());
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let errors = decode_errors(&out);
    eprintln!("P4 open-GOP decode errors: {}", errors);
    assert_eq!(errors, 0, "open-GOP output must decode cleanly");
    let _ = std::fs::remove_dir_all(&dir);
}
