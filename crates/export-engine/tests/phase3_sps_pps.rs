//! Phase 3: SPS/PPS compatibility (not byte-identical).
//!
//! The re-encode parts must remain decodable with the source's container
//! parameters.  We match profile/level (and keep pix_fmt/resolution/fps), then
//! verify the output decodes cleanly and, if available, plays in mpv.

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

fn profile_rank(id: i32) -> u32 {
    match id {
        66 | 578 => 1, // Baseline / Constrained Baseline
        77 => 2,       // Main
        100 => 3,      // High
        110 => 4,      // High 10
        122 => 5,      // High 4:2:2
        244 => 6,      // High 4:4:4
        _ => 0,
    }
}

fn video_profile_level(path: &Path) -> (i32, i32) {
    let ictx = ffmpeg_next::format::input(path).expect("open");
    let stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .expect("video stream");
    unsafe {
        let p = stream.parameters().as_ptr();
        ((*p).profile, (*p).level)
    }
}

fn count_decode_errors(path: &Path) -> usize {
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
    let mut errors = 0usize;
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
        while let Ok(_) = dec.receive_frame(&mut frame) {}
    }
    let _ = dec.send_eof();
    while let Ok(_) = dec.receive_frame(&mut frame) {}
    errors
}

fn export_profile_source(dir: &Path) -> (PathBuf, PathBuf) {
    let src = dir.join("src.mp4");
    fixtures::gen_video_profile(&src, 60, 50, "baseline", "3.0").unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, 0.0, 2.0);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into())
            .with_force_reencode(true);
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    (src, out)
}

#[test]
fn phase3_sps_pps_compatible_with_source() {
    let dir = std::env::temp_dir().join(format!("ff_p3_compat_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (src, out) = export_profile_source(&dir);
    let src_pl = video_profile_level(&src);
    let out_pl = video_profile_level(&out);
    eprintln!(
        "P3 source profile/level={:?} output={:?} ranks={}/{}",
        src_pl,
        out_pl,
        profile_rank(src_pl.0),
        profile_rank(out_pl.0)
    );
    assert!(
        profile_rank(out_pl.0) >= profile_rank(src_pl.0),
        "output profile must be at least the source profile"
    );
    assert_eq!(src_pl.1, out_pl.1, "level must match the source");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase3_output_decodes_cleanly() {
    let dir = std::env::temp_dir().join(format!("ff_p3_decode_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (_src, out) = export_profile_source(&dir);
    let errors = count_decode_errors(&out);
    eprintln!("P3 decode errors: {}", errors);
    assert_eq!(errors, 0, "output must decode without errors");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase3_output_plays_in_mpv() {
    let dir = std::env::temp_dir().join(format!("ff_p3_mpv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (_src, out) = export_profile_source(&dir);
    match std::process::Command::new("mpv")
        .args(["--no-config", "--really-quiet", "--frames=1"])
        .arg(&out)
        .status()
    {
        Ok(status) => {
            eprintln!("P3 mpv status: {:?}", status);
            assert!(status.success(), "mpv must play the output");
        }
        Err(e) => {
            eprintln!("P3 mpv not available, skipping: {e}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
