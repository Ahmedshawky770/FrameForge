//! Test-only fixture generation using libav (ffmpeg-next), never the CLI.
//!
//! Fixtures plan:
//! - [x] MP4/H.264, GOP=50, generated with libav.
//! - [x] Non-zero start_time.
//! - [x] AAC audio (mono, 48 kHz) via `gen_av`.
//! - [ ] VFR (pending: variable PTS currently trips the MP4 muxer's DTS
//!       monotonicity; needs a proper VFR timebase/encoder setup).
//! - [ ] Real .ts with non-zero start_time, MP2 audio, long GOP (pending:
//!       needs MPEG-TS muxing + MP2 encoder; not required for the 0-byte
//!       diagnosis).

use ffmpeg_next::{codec, format, frame, packet::Packet, Dictionary, Rational};
use std::path::Path;

pub fn gen_video(
    path: &Path,
    frames: usize,
    gop: u32,
    start_time: f64,
    vfr: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    gen_impl(
        path, frames, gop, start_time, vfr, false, None, None, false, None, None,
    )
}

pub fn gen_video_profile(
    path: &Path,
    frames: usize,
    gop: u32,
    profile: &str,
    level: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    gen_impl(
        path,
        frames,
        gop,
        0.0,
        false,
        false,
        Some(profile),
        Some(level),
        false,
        None,
        None,
    )
}

pub fn gen_av(
    path: &Path,
    frames: usize,
    gop: u32,
    start_time: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    gen_impl(
        path, frames, gop, start_time, false, true, None, None, false, None, None,
    )
}

pub fn gen_ts_video(
    path: &Path,
    frames: usize,
    gop: u32,
    start_time: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    ffmpeg_next::init()?;
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Fatal);
    let mut octx = ffmpeg_next::format::output_as(path, "mpegts")?;

    let codec = ffmpeg_next::codec::encoder::find(ffmpeg_next::codec::Id::H264)
        .ok_or("no H.264 encoder")?;
    let mut ost = octx.add_stream(codec)?;
    let mut enc_ctx = ffmpeg_next::codec::Context::new_with_codec(codec);
    let mut enc = enc_ctx.encoder().video()?;
    enc.set_width(320);
    enc.set_height(240);
    enc.set_format(ffmpeg_next::format::Pixel::YUV420P);
    enc.set_frame_rate(Some(ffmpeg_next::Rational(30, 1)));
    enc.set_time_base(ffmpeg_next::Rational(1, 90000));
    // No GLOBAL_HEADER -> Annex B packets for TS muxer.
    let mut opts = ffmpeg_next::Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("crf", "23");
    opts.set("g", &gop.to_string());
    let mut opened = enc.open_with(opts)?;
    ost.set_parameters(&opened);
    ost.set_time_base(ffmpeg_next::Rational(1, 90000));
    let video_stream_index = ost.index();
    drop(ost);

    octx.write_header()?;

    let mut video = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, 320, 240);
    let step: i64 = 3000; // 30 fps at 1/90000
    let base = if start_time > 0.0 {
        (start_time * 90000.0) as i64
    } else {
        0
    };
    for i in 0..frames {
        let y = video.data_mut(0);
        for (j, b) in y.iter_mut().enumerate() {
            *b = ((i * 8 + j / 16) % 255) as u8;
        }
        for plane in 1..3 {
            for b in video.data_mut(plane).iter_mut() {
                *b = 128;
            }
        }
        video.set_pts(Some(base + (i as i64) * step));
        opened.send_frame(&video)?;
        let mut pkt = ffmpeg_next::packet::Packet::empty();
        while opened.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(video_stream_index);
            pkt.set_position(-1);
            pkt.write_interleaved(&mut octx)?;
        }
    }
    opened.send_eof()?;
    let mut pkt = ffmpeg_next::packet::Packet::empty();
    while opened.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(video_stream_index);
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx)?;
    }
    octx.write_trailer()?;
    Ok(())
}

pub fn gen_video_open_gop(
    path: &Path,
    frames: usize,
    gop: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    gen_impl(
        path,
        frames,
        gop,
        0.0,
        false,
        false,
        None,
        None,
        true,
        Some("fast"),
        Some("open-gop=1:bframes=3:weightp=1"),
    )
}

pub fn gen_video_fast(
    path: &Path,
    frames: usize,
    gop: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    gen_impl(
        path,
        frames,
        gop,
        0.0,
        false,
        false,
        Some("baseline"),
        Some("1.3"),
        false,
        Some("fast"),
        Some("weightp=0:weightb=0:bframes=0"),
    )
}

fn gen_impl(
    path: &Path,
    frames: usize,
    gop: u32,
    start_time: f64,
    vfr: bool,
    with_audio: bool,
    profile: Option<&str>,
    level: Option<&str>,
    open_gop: bool,
    preset: Option<&str>,
    x264_params: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    ffmpeg_next::init()?;
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Fatal);
    let mut octx = format::output_as(path, "mp4")?;

    let codec = codec::encoder::find(codec::Id::H264).ok_or("no H.264 encoder")?;
    let mut ost = octx.add_stream(codec)?;
    let enc_ctx = codec::Context::new_with_codec(codec);
    let mut enc = enc_ctx.encoder().video()?;
    enc.set_width(320);
    enc.set_height(240);
    enc.set_format(format::Pixel::YUV420P);
    enc.set_frame_rate(Some(Rational(30, 1)));
    enc.set_time_base(Rational(1, 15360));
    enc.set_flags(ffmpeg_next::codec::Flags::GLOBAL_HEADER);

    let mut opts = Dictionary::new();
    opts.set(
        "preset",
        preset.unwrap_or(if open_gop { "fast" } else { "ultrafast" }),
    );
    opts.set("crf", "23");
    let g = gop.to_string();
    opts.set("g", &g);
    if let Some(x) = x264_params {
        opts.set("x264-params", x);
    }
    if let Some(p) = profile {
        opts.set("profile", p);
    }
    if let Some(l) = level {
        opts.set("level", l);
    }
    let mut opened = enc.open_with(opts)?;
    ost.set_parameters(&opened);
    ost.set_time_base(Rational(1, 15360));
    let video_stream_index = ost.index();
    drop(ost);

    let mut audio_stream_index: Option<usize> = None;
    let mut audio_opened = None;
    if with_audio {
        let acodec = codec::encoder::find(codec::Id::AAC).ok_or("no AAC encoder")?;
        let mut aost = octx.add_stream(acodec)?;
        let mut aenc_ctx = codec::Context::new_with_codec(acodec);
        let mut aenc = aenc_ctx.encoder().audio()?;
        aenc.set_rate(48000);
        aenc.set_channels(1);
        aenc.set_channel_layout(ffmpeg_next::channel_layout::ChannelLayout::MONO);
        aenc.set_format(ffmpeg_next::format::Sample::F32(
            ffmpeg_next::format::sample::Type::Packed,
        ));
        aenc.set_time_base(Rational(1, 48000));
        let aopened = aenc.open_with(Dictionary::new())?;
        aost.set_parameters(&aopened);
        aost.set_time_base(Rational(1, 48000));
        let idx = aost.index();
        drop(aost);
        audio_stream_index = Some(idx);
        audio_opened = Some(aopened);
    }

    octx.write_header()?;

    let mut video = frame::Video::new(format::Pixel::YUV420P, 320, 240);
    let step: i64 = 512; // 30 fps at 1/15360
    let base = if start_time > 0.0 {
        (start_time * 15360.0) as i64
    } else {
        0
    };
    for i in 0..frames {
        let y = video.data_mut(0);
        for (j, b) in y.iter_mut().enumerate() {
            *b = ((i * 8 + j / 16) % 255) as u8;
        }
        for plane in 1..3 {
            let d = video.data_mut(plane);
            for b in d.iter_mut() {
                *b = 128;
            }
        }
        let pts = base
            + if vfr && i % 3 == 0 {
                (i as i64) * step * 2
            } else {
                (i as i64) * step
            };
        video.set_pts(Some(pts));
        opened.send_frame(&video)?;
        let mut pkt = Packet::empty();
        while opened.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(video_stream_index);
            pkt.set_position(-1);
            pkt.write_interleaved(&mut octx)?;
        }
    }

    if let (Some(astream_index), Some(mut aopened)) =
        (audio_stream_index.take(), audio_opened.take())
    {
        let mut sample_pts: i64 = 0;
        let duration_frames = (frames as f64 / 30.0).ceil() as usize;
        let total_samples = duration_frames * 48000;
        let mut encoded = 0usize;
        while encoded < total_samples {
            let mut audio = frame::Audio::new(
                ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
                1024,
                ffmpeg_next::channel_layout::ChannelLayout::MONO,
            );
            audio.set_rate(48000);
            let samples = audio.samples() as usize;
            let data = audio.data_mut(0);
            let samples_slice =
                unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut f32, samples) };
            for (i, s) in samples_slice.iter_mut().enumerate() {
                let t = (sample_pts + i as i64) as f32 / 48000.0;
                *s = (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.2;
            }
            audio.set_pts(Some(sample_pts));
            sample_pts += samples as i64;
            encoded += samples;
            aopened.send_frame(&audio)?;
            let mut pkt = Packet::empty();
            while aopened.receive_packet(&mut pkt).is_ok() {
                pkt.set_stream(astream_index);
                pkt.set_position(-1);
                pkt.write_interleaved(&mut octx)?;
            }
        }
        aopened.send_eof()?;
        let mut pkt = Packet::empty();
        while aopened.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(astream_index);
            pkt.set_position(-1);
            pkt.write_interleaved(&mut octx)?;
        }
    }

    opened.send_eof()?;
    let mut pkt = Packet::empty();
    while opened.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(video_stream_index);
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx)?;
    }
    octx.write_trailer()?;
    Ok(())
}
