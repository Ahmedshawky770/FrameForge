//! Phase 0 (baseline + operational safety) and Phase 1 (0-byte diagnosis).
//!
//! These tests use libav (ffmpeg-next) for fixture generation and for reading
//! back the written MP4.  They never invoke the `ffmpeg` CLI.

mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::{plan_export, ExportPlan};
use frameforge_export_engine::{
    ensure_disk_space, CancelToken, ExportExecutor, ReencodeExecutor, StreamCopyExecutor,
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

fn make_plan(path: &Path, start: f64, end: f64, mode: ExportMode) -> ExportPlan {
    let edl = make_edl(path, start, end, mode);
    plan_export(&edl).expect("plan_export")
}

fn read_video_packet_sizes(path: &Path) -> Vec<usize> {
    let mut ictx = ffmpeg_next::format::input(path).expect("open output");
    let mut sizes = Vec::new();
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            sizes.push(pkt.size());
        }
    }
    sizes
}

fn log_packets(path: &Path, label: &str) {
    let mut ictx = ffmpeg_next::format::input(path).expect("open output");
    let stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .expect("video stream");
    let extra = unsafe {
        let p = stream.parameters().as_ptr();
        let ptr = (*p).extradata;
        let size = (*p).extradata_size;
        if ptr.is_null() || size <= 0 {
            Vec::<u8>::new()
        } else {
            std::slice::from_raw_parts(ptr, size as usize).to_vec()
        }
    };
    eprintln!(
        "{}: stream_index={} extradata_len={} extradata={:02x?}",
        label,
        stream.index(),
        extra.len(),
        &extra[..extra.len().min(24)]
    );
    for (i, (stream, pkt)) in ictx.packets().enumerate() {
        if i >= 8 {
            break;
        }
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        eprintln!(
            "{}: packet[{}] size={} data_len={:?} flags={:?} pts={:?} dts={:?}",
            label,
            i,
            pkt.size(),
            pkt.data().map(|d| d.len()),
            pkt.flags(),
            pkt.pts(),
            pkt.dts()
        );
    }
}

#[test]
fn phase0_disk_space_precheck_fails() {
    let dir = std::env::temp_dir();
    let dest = dir.join(format!("ff_disk_{}.mp4", std::process::id()));
    let err = ensure_disk_space(&dest, u64::MAX).expect_err("must fail");
    eprintln!("DISK PRECHECK: {err}");
    assert!(err.to_string().contains("disk space"));
}

#[test]
fn phase0_cancel_cleans_temp() {
    let dir = std::env::temp_dir().join(format!("ff_cancel_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 600, 50, 0.0, false).unwrap();
    let out = dir.join("out.mp4");
    let plan = make_plan(&src, 0.0, 20.0, ExportMode::StreamCopy);

    let cancel = CancelToken::new();
    let cancel2 = cancel.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1));
        cancel2.cancel();
    });
    let progress = |_p: f32| {};
    let mut exec = StreamCopyExecutor::new(
        vec![Clip {
            id: SourceId(1),
            path: src.clone(),
            segments: vec![],
        }],
        out.clone().into(),
    );
    let result = exec.run(&plan, &progress, &cancel);
    let _ = handle.join();
    eprintln!(
        "CANCEL RESULT: {:?}",
        result.as_ref().err().map(|e| e.to_string())
    );
    assert!(result.is_err(), "cancelled export must fail");

    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    eprintln!("CANCEL LEFTOVER TMP: {:?}", leftovers);
    assert!(leftovers.is_empty(), "no .tmp files may remain");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase0_fixtures_nonzero_start_time() {
    let dir = std::env::temp_dir().join(format!("ff_start_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("start.mp4");
    fixtures::gen_video(&src, 120, 50, 1.0, false).unwrap();
    let info = frameforge_media_probe::probe(&src).unwrap();
    eprintln!(
        "NON-ZERO START fixture: duration={:?} streams={}",
        info.format.duration_sec,
        info.streams.len()
    );
    assert!(info.format.duration_sec.unwrap_or(0.0) > 0.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase1_diagnostic_a_reencode_only() {
    let dir = std::env::temp_dir().join(format!("ff_diag_a_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let out = dir.join("a_reencode.mp4");
    let plan = make_plan(&src, 0.0, 4.0, ExportMode::Reencode);
    let mut exec = ReencodeExecutor::new(
        vec![Clip {
            id: SourceId(1),
            path: src.clone(),
            segments: vec![],
        }],
        out.clone().into(),
        "auto".into(),
        "fast".into(),
    );
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let sizes = read_video_packet_sizes(&out);
    eprintln!(
        "CASE A reencode-only: packets={} sizes={:?}",
        sizes.len(),
        &sizes[..sizes.len().min(12)]
    );
    log_packets(&out, "CASE A");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase1_diagnostic_b_copy_only() {
    let dir = std::env::temp_dir().join(format!("ff_diag_b_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let out = dir.join("b_copy.mp4");
    let plan = make_plan(&src, 0.0, 4.0, ExportMode::StreamCopy);
    let mut exec = StreamCopyExecutor::new(
        vec![Clip {
            id: SourceId(1),
            path: src.clone(),
            segments: vec![],
        }],
        out.clone().into(),
    );
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let sizes = read_video_packet_sizes(&out);
    eprintln!(
        "CASE B copy-only: packets={} sizes={:?}",
        sizes.len(),
        &sizes[..sizes.len().min(12)]
    );
    log_packets(&out, "CASE B");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase1_diagnostic_c_mixed() {
    let dir = std::env::temp_dir().join(format!("ff_diag_c_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();

    // Produce a re-encoded MP4 for [0,1] to obtain encoder packets.
    let reenc = dir.join("reenc.mp4");
    let re_plan = make_plan(&src, 0.0, 1.0, ExportMode::Reencode);
    let mut re_exec = ReencodeExecutor::new(
        vec![Clip {
            id: SourceId(1),
            path: src.clone(),
            segments: vec![],
        }],
        reenc.clone().into(),
        "auto".into(),
        "fast".into(),
    );
    re_exec.run(&re_plan, &|_| {}, &CancelToken::new()).unwrap();

    let out = dir.join("c_mixed.mp4");
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let mut rctx = ffmpeg_next::format::input(&reenc).unwrap();
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();

    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    unsafe {
        (*ost.parameters().as_mut_ptr()).codec_tag = 0;
    }
    octx.write_header().unwrap();

    let mut copied = 0;
    let mut last_pts = 0i64;
    let mut last_dts = 0i64;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if copied >= 30 {
            break;
        }
        pkt.set_stream(0);
        pkt.set_position(-1);
        last_pts = pkt.pts().unwrap_or(last_pts);
        last_dts = pkt.dts().unwrap_or(last_dts);
        eprintln!(
            "MIX copy-before: size={} data_len={:?} key={}",
            pkt.size(),
            pkt.data().map(|d| d.len()),
            pkt.is_key()
        );
        if let Err(e) = pkt.write_interleaved(&mut octx) {
            eprintln!("MIX copy write error: {e}");
            break;
        }
        copied += 1;
    }

    for (stream, mut pkt) in rctx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        pkt.set_pts(Some(pkt.pts().unwrap_or(0) + last_pts + 1));
        pkt.set_dts(Some(pkt.dts().unwrap_or(0) + last_dts + 1));
        pkt.set_stream(0);
        pkt.set_position(-1);
        eprintln!(
            "MIX reenc-before: size={} data_len={:?} key={}",
            pkt.size(),
            pkt.data().map(|d| d.len()),
            pkt.is_key()
        );
        if let Err(e) = pkt.write_interleaved(&mut octx) {
            eprintln!("MIX reenc write error: {e}");
            break;
        }
    }
    let _ = octx.write_trailer();

    let sizes = read_video_packet_sizes(&out);
    eprintln!(
        "CASE C mixed: packets={} sizes={:?}",
        sizes.len(),
        &sizes[..sizes.len().min(12)]
    );
    log_packets(&out, "CASE C");
    let _ = std::fs::remove_dir_all(&dir);
}
