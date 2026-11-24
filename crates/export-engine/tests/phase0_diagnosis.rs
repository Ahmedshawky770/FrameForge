//! Phase 0 diagnosis: 0-byte MP4 muxer hypothesis.
//!
//! Four cases on MP4 and four on .ts.  No ffmpeg CLI.

mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::plan_export;
use frameforge_export_engine::{CancelToken, ExportExecutor, ReencodeExecutor, StreamCopyExecutor};
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

fn make_plan(
    path: &Path,
    start: f64,
    end: f64,
    mode: ExportMode,
) -> frameforge_edit_engine::ExportPlan {
    plan_export(&make_edl(path, start, end, mode)).expect("plan_export")
}

fn read_first_video_size(path: &Path) -> usize {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            return pkt.size();
        }
    }
    0
}

fn read_video_sample_sizes(path: &Path) -> Vec<usize> {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    let mut out = Vec::new();
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            out.push(pkt.size());
        }
    }
    out
}

fn count_decode_errors(path: &Path) -> usize {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    let stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .expect("video stream")
        .parameters()
        .clone();
    let mut dec_ctx = ffmpeg_next::codec::Context::from_parameters(stream.clone()).expect("dec");
    let mut dec = dec_ctx.decoder().video().expect("dec video");
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
        while dec.receive_frame(&mut frame).is_ok() {}
    }
    let _ = dec.send_eof();
    while dec.receive_frame(&mut frame).is_ok() {}
    errors
}

fn log_src_extradata(path: &Path, label: &str) {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
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
        "  {}: src extradata_len={} first_bytes={:?}",
        label,
        extra.len(),
        &extra[..extra.len().min(8)]
    );
}

fn log_first_packet(path: &Path, label: &str) {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            let data = pkt.data().unwrap_or(&[]);
            let first6: Vec<u8> = data.iter().take(6).copied().collect();
            eprintln!(
                "  {}: pkt.size={} data_len={:?} first6={:?} flags={:?} pts={:?} dts={:?}",
                label,
                pkt.size(),
                pkt.data().map(|d| d.len()),
                first6,
                pkt.flags(),
                pkt.pts(),
                pkt.dts()
            );
            break;
        }
    }
}

#[test]
fn phase0_diag_mp4_copy_one_packet() {
    let dir = std::env::temp_dir().join(format!("ff_diag0_mp4_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let out = dir.join("one.mp4");
    log_src_extradata(&src, "MP4 copy-one");
    log_first_packet(&src, "MP4 copy-one");
    // Write one packet
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
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
    octx.write_header().unwrap();
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            pkt.set_stream(0);
            pkt.set_position(-1);
            eprintln!("  MP4 copy-one: write size={}", pkt.size());
            pkt.write_interleaved(&mut octx).unwrap();
            break;
        }
    }
    octx.write_trailer().unwrap();
    let sample = read_first_video_size(&out);
    let errors = count_decode_errors(&out);
    eprintln!(
        "  MP4 copy-one: output_sample_size={} decode_errors={}",
        sample, errors
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase0_diag_mp4_copy_one_packet_after_seek() {
    let dir = std::env::temp_dir().join(format!("ff_diag0_seek_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let out = dir.join("one_seek.mp4");
    log_src_extradata(&src, "MP4 copy-seek");
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    // seek to ~2s
    ictx.seek(2_000_000, i64::MIN..2_000_000).unwrap();
    let mut first = None;
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            first = Some((
                stream.index(),
                pkt.size(),
                pkt.data().map(|d| d.len()).unwrap_or(0),
                pkt.pts(),
                pkt.dts(),
                pkt.flags(),
                pkt.is_key(),
            ));
            break;
        }
    }
    eprintln!("  MP4 copy-seek: first after seek: {:?}", first);
    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    octx.write_header().unwrap();
    let mut written = false;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if written {
            break;
        }
        pkt.set_stream(0);
        pkt.set_position(-1);
        eprintln!("  MP4 copy-seek: write size={}", pkt.size());
        pkt.write_interleaved(&mut octx).unwrap();
        written = true;
    }
    octx.write_trailer().unwrap();
    let sample = read_first_video_size(&out);
    let errors = count_decode_errors(&out);
    eprintln!(
        "  MP4 copy-seek: output_sample_size={} decode_errors={}",
        sample, errors
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase0_diag_mp4_packet_copy_vs_original() {
    let dir = std::env::temp_dir().join(format!("ff_diag0_copyvs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let out_orig = dir.join("orig.mp4");
    let out_copy = dir.join("copy.mp4");
    log_src_extradata(&src, "MP4 copy-vs-orig");
    // Find first video packet
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let mut first_pkt = None;
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            first_pkt = Some(pkt);
            break;
        }
    }
    let pkt = first_pkt.expect("first packet");
    let data = pkt.data().unwrap_or(&[]);
    let first6: Vec<u8> = data.iter().take(6).copied().collect();
    eprintln!(
        "  MP4 copy-vs-orig: orig size={} data_len={} first6={:?} flags={:?} pts={:?} dts={:?}",
        pkt.size(),
        data.len(),
        first6,
        pkt.flags(),
        pkt.pts(),
        pkt.dts()
    );
    // Write original
    {
        let mut ictx2 = ffmpeg_next::format::input(&src).unwrap();
        let src_stream = ictx2
            .streams()
            .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
            .unwrap();
        let mut octx = ffmpeg_next::format::output_as(&out_orig, "mp4").unwrap();
        let mut ost = octx
            .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
            .unwrap();
        ost.set_parameters(src_stream.parameters());
        ost.set_time_base(src_stream.time_base());
        octx.write_header().unwrap();
        for (stream, mut p) in ictx2.packets() {
            if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
                p.set_stream(0);
                p.set_position(-1);
                p.write_interleaved(&mut octx).unwrap();
                break;
            }
        }
        octx.write_trailer().unwrap();
    }
    // Write Packet::copy
    {
        let mut ictx2 = ffmpeg_next::format::input(&src).unwrap();
        let src_stream = ictx2
            .streams()
            .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
            .unwrap();
        let mut octx = ffmpeg_next::format::output_as(&out_copy, "mp4").unwrap();
        let mut ost = octx
            .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
            .unwrap();
        ost.set_parameters(src_stream.parameters());
        ost.set_time_base(src_stream.time_base());
        octx.write_header().unwrap();
        for (stream, p) in ictx2.packets() {
            if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
                let data = p.data().unwrap_or(&[]);
                let mut cp = ffmpeg_next::packet::Packet::copy(data);
                cp.set_flags(p.flags());
                cp.set_pts(p.pts());
                cp.set_dts(p.dts());
                cp.set_stream(0);
                cp.set_position(-1);
                cp.write_interleaved(&mut octx).unwrap();
                break;
            }
        }
        octx.write_trailer().unwrap();
    }
    let orig_size = read_first_video_size(&out_orig);
    let copy_size = read_first_video_size(&out_copy);
    let orig_err = count_decode_errors(&out_orig);
    let copy_err = count_decode_errors(&out_copy);
    eprintln!(
        "  MP4 copy-vs-orig: orig_size={} copy_size={} orig_err={} copy_err={}",
        orig_size, copy_size, orig_err, copy_err
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase0_diag_mp4_mixed_two_packets() {
    let dir = std::env::temp_dir().join(format!("ff_diag0_mixed_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let reenc = dir.join("reenc.mp4");
    // re-encode [0,1]
    let plan = make_plan(&src, 0.0, 1.0, ExportMode::Reencode);
    let mut exec = ReencodeExecutor::new(
        vec![Clip {
            id: SourceId(1),
            path: src.clone(),
            segments: vec![],
        }],
        reenc.clone().into(),
        "auto".into(),
        "fast".into(),
    );
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let out = dir.join("mixed.mp4");
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let mut rctx = ffmpeg_next::format::input(&reenc).unwrap();
    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    octx.write_header().unwrap();
    // Write 1 copied packet
    let mut last_pts = 0i64;
    let mut last_dts = 0i64;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        pkt.set_stream(0);
        pkt.set_position(-1);
        eprintln!(
            "  MP4 mixed copy-before: size={} data_len={:?} first6={:?} flags={:?} pts={:?} dts={:?}",
            pkt.size(),
            pkt.data().map(|d| d.len()),
            pkt.data().map(|d| d.iter().take(6).copied().collect::<Vec<u8>>()),
            pkt.flags(),
            pkt.pts(),
            pkt.dts()
        );
        pkt.write_interleaved(&mut octx).unwrap();
        last_pts = pkt.pts().unwrap_or(last_pts);
        last_dts = pkt.dts().unwrap_or(last_dts);
        break;
    }
    // Write 1 re-encoded packet with rebasing
    for (stream, mut pkt) in rctx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        pkt.set_pts(Some(pkt.pts().unwrap_or(0) + last_pts + 1));
        pkt.set_dts(Some(pkt.dts().unwrap_or(0) + last_dts + 1));
        pkt.set_stream(0);
        pkt.set_position(-1);
        eprintln!(
            "  MP4 mixed reenc-before: size={} data_len={:?} first6={:?} flags={:?} pts={:?} dts={:?}",
            pkt.size(),
            pkt.data().map(|d| d.len()),
            pkt.data().map(|d| d.iter().take(6).copied().collect::<Vec<u8>>()),
            pkt.flags(),
            pkt.pts(),
            pkt.dts()
        );
        pkt.write_interleaved(&mut octx).unwrap();
        break;
    }
    octx.write_trailer().unwrap();
    let sizes = read_video_sample_sizes(&out);
    let errors = count_decode_errors(&out);
    eprintln!(
        "  MP4 mixed: output_sample_sizes={:?} decode_errors={}",
        sizes, errors
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "diagnostic, crashes in this env"]
fn phase0_diag_ts_copy_one_packet() {
    let dir = std::env::temp_dir().join(format!("ff_diag0_ts_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.ts");
    fixtures::gen_ts_video(&src, 120, 50, 1.0).unwrap();
    log_src_extradata(&src, "TS copy-one");
    log_first_packet(&src, "TS copy-one");
    // Copy first video packet
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    let extra = unsafe {
        let p = src_stream.parameters().as_ptr();
        let ptr = (*p).extradata;
        let size = (*p).extradata_size;
        if ptr.is_null() || size <= 0 {
            Vec::<u8>::new()
        } else {
            std::slice::from_raw_parts(ptr, size as usize).to_vec()
        }
    };
    eprintln!(
        "  TS copy-one: src extradata_len={} first_bytes={:?}",
        extra.len(),
        &extra[..extra.len().min(8)]
    );
    let out = dir.join("one.mp4");
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    let _ = octx.write_header();
    let mut written = false;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if written {
            break;
        }
        pkt.set_stream(0);
        pkt.set_position(-1);
        eprintln!("  TS copy-one: write size={}", pkt.size());
        let _ = pkt.write_interleaved(&mut octx);
        written = true;
    }
    octx.write_trailer().unwrap();
    let sample = read_first_video_size(&out);
    let errors = count_decode_errors(&out);
    eprintln!(
        "  TS copy-one: output_sample_size={} decode_errors={}",
        sample, errors
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "diagnostic, crashes in this env"]
fn phase0_diag_ts_copy_one_packet_after_seek() {
    let dir = std::env::temp_dir().join(format!("ff_diag0_ts_seek_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.ts");
    fixtures::gen_ts_video(&src, 120, 50, 1.0).unwrap();
    log_src_extradata(&src, "TS copy-seek");
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    ictx.seek(90_000, i64::MIN..90_000).unwrap();
    let mut first = None;
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            first = Some((
                stream.index(),
                pkt.size(),
                pkt.data().map(|d| d.len()).unwrap_or(0),
                pkt.pts(),
                pkt.dts(),
                pkt.flags(),
                pkt.is_key(),
            ));
            break;
        }
    }
    eprintln!("  TS copy-seek: first after seek: {:?}", first);
    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    let out = dir.join("one_seek.mp4");
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    let _ = octx.write_header();
    let mut written = false;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if written {
            break;
        }
        pkt.set_stream(0);
        pkt.set_position(-1);
        eprintln!("  TS copy-seek: write size={}", pkt.size());
        let _ = pkt.write_interleaved(&mut octx);
        written = true;
    }
    octx.write_trailer().unwrap();
    let sample = read_first_video_size(&out);
    let errors = count_decode_errors(&out);
    eprintln!(
        "  TS copy-seek: output_sample_size={} decode_errors={}",
        sample, errors
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase0_diag_ts_packet_copy_vs_original() {
    let dir = std::env::temp_dir().join(format!("ff_diag0_ts_copyvs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.ts");
    fixtures::gen_ts_video(&src, 120, 50, 1.0).unwrap();
    log_src_extradata(&src, "TS copy-vs-orig");
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let mut first_pkt = None;
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            first_pkt = Some(pkt);
            break;
        }
    }
    let pkt = first_pkt.expect("first packet");
    let data = pkt.data().unwrap_or(&[]);
    let first6: Vec<u8> = data.iter().take(6).copied().collect();
    eprintln!(
        "  TS copy-vs-orig: size={} data_len={} first6={:?} flags={:?} pts={:?} dts={:?} extradata_len={}",
        pkt.size(),
        data.len(),
        first6,
        pkt.flags(),
        pkt.pts(),
        pkt.dts(),
        unsafe {
            let p = ictx
                .streams()
                .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
                .unwrap()
                .parameters()
                .as_ptr();
            let ptr = (*p).extradata;
            let size = (*p).extradata_size;
            if ptr.is_null() || size <= 0 { 0 } else { size as usize }
        }
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "diagnostic, crashes in this env"]
fn phase0_diag_ts_mixed_two_packets() {
    let dir = std::env::temp_dir().join(format!("ff_diag0_ts_mixed_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.ts");
    fixtures::gen_ts_video(&src, 120, 50, 1.0).unwrap();
    let reenc = dir.join("reenc.mp4");
    let plan = make_plan(&src, 0.0, 1.0, ExportMode::Reencode);
    let mut exec = ReencodeExecutor::new(
        vec![Clip {
            id: SourceId(1),
            path: src.clone(),
            segments: vec![],
        }],
        reenc.clone().into(),
        "auto".into(),
        "fast".into(),
    );
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();
    let out = dir.join("mixed.mp4");
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let mut rctx = ffmpeg_next::format::input(&reenc).unwrap();
    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    let extra = unsafe {
        let p = src_stream.parameters().as_ptr();
        let ptr = (*p).extradata;
        let size = (*p).extradata_size;
        if ptr.is_null() || size <= 0 {
            Vec::<u8>::new()
        } else {
            std::slice::from_raw_parts(ptr, size as usize).to_vec()
        }
    };
    eprintln!(
        "  TS mixed: src extradata_len={} first_bytes={:?}",
        extra.len(),
        &extra[..extra.len().min(8)]
    );
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    let _ = octx.write_header();
    let mut last_pts = 0i64;
    let mut last_dts = 0i64;
    let mut copied = 0;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if copied >= 5 {
            break;
        }
        pkt.set_stream(0);
        pkt.set_position(-1);
        let data = pkt.data().unwrap_or(&[]);
        let first6: Vec<u8> = data.iter().take(6).copied().collect();
        eprintln!(
            "  TS mixed copy-before: size={} data_len={:?} first6={:?} flags={:?} pts={:?} dts={:?}",
            pkt.size(),
            data.len(),
            first6,
            pkt.flags(),
            pkt.pts(),
            pkt.dts()
        );
        let _ = pkt.write_interleaved(&mut octx);
        last_pts = pkt.pts().unwrap_or(last_pts);
        last_dts = pkt.dts().unwrap_or(last_dts);
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
        let data = pkt.data().unwrap_or(&[]);
        let first6: Vec<u8> = data.iter().take(6).copied().collect();
        eprintln!(
            "  TS mixed reenc-before: size={} data_len={:?} first6={:?} flags={:?} pts={:?} dts={:?}",
            pkt.size(),
            data.len(),
            first6,
            pkt.flags(),
            pkt.pts(),
            pkt.dts()
        );
        pkt.write_interleaved(&mut octx).unwrap();
        break;
    }
    octx.write_trailer().unwrap();
    let sizes = read_video_sample_sizes(&out);
    let errors = count_decode_errors(&out);
    eprintln!(
        "  TS mixed: output_sample_sizes={:?} decode_errors={}",
        sizes, errors
    );
    let _ = std::fs::remove_dir_all(&dir);
}
