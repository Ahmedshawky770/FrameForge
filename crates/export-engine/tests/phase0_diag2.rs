//! Phase 0 diagnosis round 2: size mismatches, causality, .ts, 100-packet mixed.
//! No ffmpeg CLI.

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

fn full_extradata(path: &Path) -> Vec<u8> {
    let mut ictx = ffmpeg_next::format::input(path).expect("open");
    let stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .expect("video stream");
    unsafe {
        let p = stream.parameters().as_ptr();
        let ptr = (*p).extradata;
        let size = (*p).extradata_size;
        if ptr.is_null() || size <= 0 {
            Vec::<u8>::new()
        } else {
            std::slice::from_raw_parts(ptr, size as usize).to_vec()
        }
    }
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

// Test 1: MP4 copy-one (re-verify, same packet tracked)
#[test]
fn phase0d_mp4_copy_one() {
    let dir = std::env::temp_dir().join(format!("ff_d0_mp4_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let out = dir.join("one.mp4");
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    unsafe {
        let p = src_stream.parameters().as_ptr();
        let extradata = (*p).extradata;
        let size = (*p).extradata_size;
        if !extradata.is_null() && size > 0 {
            let data = std::slice::from_raw_parts(extradata, size as usize);
            eprintln!(
                "MIXED src extradata first8={:?}",
                &data[..data.len().min(8)]
            );
        }
    }
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    octx.write_header().unwrap();
    let mut first_size_before = 0usize;
    let mut written_size = 0usize;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        first_size_before = pkt.size();
        let data = pkt.data().unwrap_or(&[]);
        let first6: Vec<u8> = data.iter().take(6).copied().collect();
        eprintln!(
            "MP4-copy-one: before_write size={} data_len={} first6={:?} flags={:?} pts={:?} dts={:?}",
            pkt.size(),
            data.len(),
            first6,
            pkt.flags(),
            pkt.pts(),
            pkt.dts()
        );
        let data = pkt.data().unwrap_or(&[]).to_vec();
        let mut cp = ffmpeg_next::packet::Packet::copy(&data);
        cp.set_stream(0);
        cp.set_position(-1);
        cp.write_interleaved(&mut octx).unwrap();
        written_size = pkt.size();
        break;
    }
    octx.write_trailer().unwrap();
    let sample = read_first_video_size(&out);
    let errors = count_decode_errors(&out);
    eprintln!(
        "MP4-copy-one: size_before_write={} written_size={} output_sample={} decode_errors={}",
        first_size_before, written_size, sample, errors
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// Test 2: MP4 copy-after-seek (re-seek before write to fix consumed packet)
#[test]
fn phase0d_mp4_copy_after_seek() {
    let dir = std::env::temp_dir().join(format!("ff_d0_seek_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let out = dir.join("seek.mp4");
    // First: find the keyframe PTS after seek (scan without consuming)
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    ictx.seek(2_000_000, i64::MIN..2_000_000).unwrap();
    let mut target_pts = None;
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video && pkt.is_key() {
            target_pts = pkt.pts();
            break;
        }
    }
    let seek_pts = target_pts.unwrap_or(0);
    // Re-open and seek to the exact keyframe
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    ictx.seek(seek_pts, i64::MIN..seek_pts).unwrap();
    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    unsafe {
        let p = src_stream.parameters().as_ptr();
        let extradata = (*p).extradata;
        let size = (*p).extradata_size;
        if !extradata.is_null() && size > 0 {
            let data = std::slice::from_raw_parts(extradata, size as usize);
            eprintln!(
                "MIXED src extradata first8={:?}",
                &data[..data.len().min(8)]
            );
        }
    }
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    octx.write_header().unwrap();
    let mut written = false;
    let mut size_before = 0usize;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if written {
            break;
        }
        if !pkt.is_key() {
            continue;
        }
        size_before = pkt.size();
        let data = pkt.data().unwrap_or(&[]);
        let first6: Vec<u8> = data.iter().take(6).copied().collect();
        eprintln!(
            "MP4-copy-seek: pkt.size={} data_len={} first6={:?} flags={:?} pts={:?} dts={:?}",
            pkt.size(),
            data.len(),
            first6,
            pkt.flags(),
            pkt.pts(),
            pkt.dts()
        );
        pkt.set_stream(0);
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).unwrap();
        eprintln!("MP4-copy-seek: write size={}", pkt.size());
        written = true;
    }
    octx.write_trailer().unwrap();
    let sample = read_first_video_size(&out);
    let errors = count_decode_errors(&out);
    eprintln!(
        "MP4-copy-seek: size_before_write={} output_sample={} decode_errors={}",
        size_before, sample, errors
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// Test 3: MP4 packet-copy vs original (already done, re-verify with same-packet tracking)
#[test]
fn phase0d_mp4_packet_copy_vs_original() {
    let dir = std::env::temp_dir().join(format!("ff_d0_copyvs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 120, 50, 0.0, false).unwrap();
    let out_orig = dir.join("orig.mp4");
    let out_copy = dir.join("copy.mp4");
    // Find first video packet and log
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let mut first_pkt_info = None;
    for (stream, pkt) in ictx.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            let data = pkt.data().unwrap_or(&[]);
            first_pkt_info = Some((
                pkt.size(),
                data.len(),
                data.iter().take(6).copied().collect::<Vec<u8>>(),
                pkt.flags(),
                pkt.pts(),
                pkt.dts(),
            ));
            break;
        }
    }
    let (sz, dl, f6, fl, pts, dts) = first_pkt_info.unwrap();
    eprintln!(
        "MP4-copy-vs-orig: orig pkt.size={} data_len={} first6={:?} flags={:?} pts={:?} dts={:?}",
        sz, dl, f6, fl, pts, dts
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
                eprintln!("MP4-copy-vs-orig: write orig size={}", p.size());
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
                eprintln!("MP4-copy-vs-orig: write copy size={}", cp.size());
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
        "MP4-copy-vs-orig: orig_output_sample={} copy_output_sample={} orig_err={} copy_err={}",
        orig_size, copy_size, orig_err, copy_err
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// Test 4: MP4 mixed 100 packets
#[test]
fn phase0d_mp4_mixed_100() {
    let dir = std::env::temp_dir().join(format!("ff_d0_mixed100_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_video(&src, 200, 50, 0.0, false).unwrap();
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
    unsafe {
        let p = src_stream.parameters().as_ptr();
        let extradata = (*p).extradata;
        let size = (*p).extradata_size;
        if !extradata.is_null() && size > 0 {
            let data = std::slice::from_raw_parts(extradata, size as usize);
            eprintln!(
                "MIXED src extradata first8={:?}",
                &data[..data.len().min(8)]
            );
        }
    }
    let mut octx = ffmpeg_next::format::output_as(&out, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    octx.write_header().unwrap();
    let mut last_pts = 0i64;
    let mut last_dts = 0i64;
    let mut copied = 0;
    let mut zero_before = 0usize;
    for (stream, mut pkt) in ictx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if copied >= 100 {
            break;
        }
        if copied == 0 {
            let d = pkt.data().unwrap_or(&[]);
            eprintln!(
                "MIXED src first pkt first8={:?} size={}",
                &d[..d.len().min(8)],
                d.len()
            );
        }
        if pkt.size() == 0 {
            zero_before += 1;
        }
        pkt.set_stream(0);
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).unwrap();
        last_pts = pkt.pts().unwrap_or(last_pts);
        last_dts = pkt.dts().unwrap_or(last_dts);
        copied += 1;
    }
    let mut reenc_count = 0;
    for (stream, mut pkt) in rctx.packets() {
        if stream.parameters().medium() != ffmpeg_next::media::Type::Video {
            continue;
        }
        if reenc_count == 0 {
            let d = pkt.data().unwrap_or(&[]);
            let mut types = Vec::new();
            let mut i = 0;
            while i < d.len() {
                if i + 3 < d.len() && d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
                    types.push(d[i + 3] & 0x1f);
                    i += 4;
                } else if i + 4 < d.len()
                    && d[i] == 0
                    && d[i + 1] == 0
                    && d[i + 2] == 0
                    && d[i + 3] == 1
                {
                    types.push(d[i + 4] & 0x1f);
                    i += 5;
                } else {
                    i += 1;
                }
            }
            eprintln!(
                "MIXED reenc first pkt first8={:?} size={}",
                &d[..d.len().min(8)],
                d.len()
            );
        }
        pkt.set_pts(Some(pkt.pts().unwrap_or(0) + last_pts + 1));
        pkt.set_dts(Some(pkt.dts().unwrap_or(0) + last_dts + 1));
        pkt.set_stream(0);
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).ok();
        reenc_count += 1;
        if reenc_count >= 10 {
            break;
        }
    }
    octx.write_trailer().unwrap();
    let sizes = read_video_sample_sizes(&out);
    let errors = count_decode_errors(&out);
    let zero_after = sizes.iter().filter(|s| **s == 0).count();
    eprintln!(
        "MP4-mixed-100: copied={} reenc={} total_out={} zero_before_write={} zero_after_write={} decode_errors={}",
        copied,
        reenc_count,
        sizes.len(),
        zero_before,
        zero_after,
        errors
    );
    eprintln!(
        "MP4-mixed-100: first10_sizes={:?}",
        &sizes[..sizes.len().min(10)]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// TS tests: print extradata + try write_header video-only and audio-only
#[test]
fn phase0d_ts_extradata_and_write_header() {
    let dir = std::env::temp_dir().join(format!("ff_d0_ts_hdr_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.ts");
    fixtures::gen_ts_video(&src, 120, 50, 1.0).unwrap();
    let mut ictx = ffmpeg_next::format::input(&src).unwrap();
    let src_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .unwrap();
    unsafe {
        let p = src_stream.parameters().as_ptr();
        let extradata = (*p).extradata;
        let size = (*p).extradata_size;
        if !extradata.is_null() && size > 0 {
            let data = std::slice::from_raw_parts(extradata, size as usize);
            eprintln!(
                "MIXED src extradata first8={:?}",
                &data[..data.len().min(8)]
            );
        }
    }
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
    eprintln!("TS-extradata: len={} hex={:02x?}", extra.len(), extra);
    // Try video-only
    let out_v = dir.join("v_only.mp4");
    let mut octx = ffmpeg_next::format::output_as(&out_v, "mp4").unwrap();
    let mut ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .unwrap();
    ost.set_parameters(src_stream.parameters());
    ost.set_time_base(src_stream.time_base());
    match octx.write_header() {
        Ok(_) => {
            eprintln!("TS-extradata: video-only write_header OK");
            octx.write_trailer().unwrap();
        }
        Err(e) => eprintln!("TS-extradata: video-only write_header FAILED: {e}"),
    }
    // Try audio-only
    let audio_stream = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Audio);
    if let Some(as_) = audio_stream {
        let out_a = dir.join("a_only.mp4");
        let mut octx = ffmpeg_next::format::output_as(&out_a, "mp4").unwrap();
        let mut ost = octx
            .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
            .unwrap();
        ost.set_parameters(as_.parameters());
        ost.set_time_base(as_.time_base());
        match octx.write_header() {
            Ok(_) => {
                eprintln!("TS-extradata: audio-only write_header OK");
                octx.write_trailer().unwrap();
            }
            Err(e) => eprintln!("TS-extradata: audio-only write_header FAILED: {e}"),
        }
    } else {
        eprintln!("TS-extradata: no audio stream found");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
