//! Smart rendering: copy keyframe-aligned GOPs and re-encode only boundary GOPs.
//!
//! A full re-encode is the slowest correct option: every frame is decoded and
//! passed through libx264.  Smart rendering keeps the compressed packets for
//! every GOP that lies completely inside a kept range and only re-encodes the
//! GOPs that contain a cut point.  The copied packets are bit-identical to the
//! source, so this is both faster and higher quality than a full re-encode.
//!
//! Correctness requirements handled here:
//!
//! * **Frame accuracy** – a cut inside a GOP re-encodes that GOP and discards
//!   frames outside `[start, end)` before encoding, so the output starts on the
//!   exact requested frame.
//! * **No parameter-set corruption** – copied and re-encoded packets may carry
//!   different SPS/PPS (different rate-control settings produce different PPS).
//!   MP4 `avc1` cannot express that.  When the encoder's extradata differs from
//!   the source's, the output uses the `avc3` sample entry and every keyframe
//!   is prefixed with its own in-band SPS/PPS.
//! * **Continuous timestamps** – the output timeline is a concatenation of the
//!   kept ranges.  Each part starts at the current output position; encoder
//!   PTS/DTS are rebased onto it.  The source start time is never added to the
//!   output timeline (that bug produced multi-minute gaps for multi-segment
//!   re-encodes).
//! * **Audio sync** – audio is re-encoded for the whole output with a single
//!   continuous sample counter.  Audio is cheap next to video and this avoids
//!   AAC priming/padding gaps at every cut.

use crate::{
    atomic_write, normalize_video_frame, open_video_encoder, output_format_from_extension,
    parse_pixel_format, resolve_video_encoder, select_audio_encoder, set_last_encoder_report,
    software_h264, CancelToken, EncoderReport, ExportExecutor, ProgressCallback,
    VideoEncoderParams, VideoEncoderSpec,
};
use ffmpeg_next::{codec, encoder, ffi, format, media, packet, Dictionary};
use frameforge_core_types::{Clip, FrameForgeError, PathBuf, Result, SourceId};
use frameforge_edit_engine::ExportPlan;
use frameforge_media_probe::{build_keyframe_index_cached, KeyframeIndex};
use std::collections::HashMap;
use std::path::Path;

/// `MKTAG('a','v','c','1')`.
const AVC1_TAG: u32 = 0x3163_7661;
const AVC3_TAG: u32 = 0x3363_7661;

const EPS: f64 = 1e-9;
const ALIGN_EPS: f64 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartKind {
    Copy,
    Reencode,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Part {
    pub(crate) kind: PartKind,
    /// Media-relative seconds.
    pub(crate) start: f64,
    pub(crate) end: f64,
}

impl Part {
    fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

struct AudioTranscoder {
    decoder: codec::decoder::Audio,
    encoder: encoder::Audio,
    ost_index: usize,
    time_base: ffmpeg_next::Rational,
    next_pts: i64,
    resampler: Option<ffmpeg_next::software::resampling::Context>,
    /// Fixed number of samples the encoder expects per frame (0 = variable).
    frame_size: usize,
    fifo_format: ffmpeg_next::format::Sample,
    fifo_layout: ffmpeg_next::channel_layout::ChannelLayout,
    /// Buffers decoded/resampled samples until a complete encoder frame is
    /// available.  Source codecs such as MP2 emit 1152-sample frames while
    /// AAC expects 1024; passing those frames straight to `send_frame` fails
    /// with EINVAL at the very end of an otherwise successful export.
    fifo: Option<AudioFifo>,
}

struct AudioFifo {
    ptr: *mut ffi::AVAudioFifo,
}

impl AudioFifo {
    fn new(format: ffmpeg_next::format::Sample, channels: i32) -> Result<Self> {
        if format == ffmpeg_next::format::Sample::None || channels <= 0 {
            return Err(FrameForgeError::Export(
                "invalid audio FIFO configuration".into(),
            ));
        }
        let ptr = unsafe { ffi::av_audio_fifo_alloc(format.into(), channels, 1) };
        if ptr.is_null() {
            Err(FrameForgeError::Export(
                "failed to allocate the audio FIFO".into(),
            ))
        } else {
            Ok(Self { ptr })
        }
    }

    fn write_frame(&mut self, frame: &ffmpeg_next::util::frame::Audio) -> Result<()> {
        let samples = frame.samples();
        if samples == 0 {
            return Ok(());
        }

        let data = unsafe { (*frame.as_ptr()).extended_data as *mut *mut libc::c_void };
        let written = unsafe { ffi::av_audio_fifo_write(self.ptr, data, samples as libc::c_int) };
        if written < 0 {
            return Err(FrameForgeError::Export(format!(
                "audio FIFO write failed: {}",
                ffmpeg_next::Error::from(written)
            )));
        }
        if written as usize != samples {
            return Err(FrameForgeError::Export(format!(
                "audio FIFO wrote {written} of {samples} samples"
            )));
        }
        Ok(())
    }

    fn size(&self) -> usize {
        let size = unsafe { ffi::av_audio_fifo_size(self.ptr) };
        size.max(0) as usize
    }

    fn read_into(
        &mut self,
        frame: &mut ffmpeg_next::util::frame::Audio,
        samples: usize,
    ) -> Result<usize> {
        let data = unsafe { (*frame.as_mut_ptr()).extended_data as *mut *mut libc::c_void };
        let read = unsafe { ffi::av_audio_fifo_read(self.ptr, data, samples as libc::c_int) };
        if read < 0 {
            Err(FrameForgeError::Export(format!(
                "audio FIFO read failed: {}",
                ffmpeg_next::Error::from(read)
            )))
        } else {
            Ok(read as usize)
        }
    }
}

impl Drop for AudioFifo {
    fn drop(&mut self) {
        unsafe { ffi::av_audio_fifo_free(self.ptr) };
    }
}

/// Check whether the first video packet at or after `start` is a real IDR
/// (NAL type 5).  Returns false for open-GOP sources where the keyframe is
/// an I-frame that references a previous GOP.

/// Convert an Annex B H.264 byte stream to AVCC (length-prefixed) NAL units.
fn annexb_to_avcc(data: &[u8]) -> Vec<u8> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i..].starts_with(&[0, 0, 0, 1]) {
            starts.push(i);
            i += 4;
        } else if data[i..].starts_with(&[0, 0, 1]) {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    if starts.is_empty() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    for (idx, &start) in starts.iter().enumerate() {
        let sc_len = if data[start..].starts_with(&[0, 0, 0, 1]) {
            4
        } else {
            3
        };
        let nal_start = start + sc_len;
        let nal_end = starts.get(idx + 1).copied().unwrap_or(data.len());
        let nal = &data[nal_start..nal_end];
        if nal.is_empty() {
            continue;
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

fn parse_pps_debug(pps: &[u8]) -> (u32, u32, u32, u32, u32) {
    let rbsp = nal_rbsp(&pps[1..]);
    let mut r = BitReader::new(&rbsp);
    let pps_id = r.ue().unwrap_or(0);
    let sps_id = r.ue().unwrap_or(0);
    let entropy = r.u(1).unwrap_or(0);
    let bottom = r.u(1).unwrap_or(0);
    let slice_groups = r.ue().unwrap_or(0);
    let ref_l0 = r.ue().unwrap_or(0);
    let ref_l1 = r.ue().unwrap_or(0);
    let wp = r.u(1).unwrap_or(0);
    let wbp = r.u(2).unwrap_or(0);
    (pps_id, sps_id, entropy, ref_l0, wp)
}
pub(crate) fn first_packet_is_idr(
    ictx: &mut format::context::Input,
    video_ist_index: usize,
    start_offset_seconds: f64,
    start: f64,
) -> bool {
    let seek_target_seconds = start_offset_seconds + start;
    let seek_pts = (seek_target_seconds * 1_000_000.0) as i64;
    if ictx.seek(seek_pts, i64::MIN..seek_pts).is_err() {
        return false;
    }
    for (stream, pkt) in ictx.packets() {
        if stream.index() != video_ist_index {
            continue;
        }
        let data = pkt.data().unwrap_or(&[]);
        // Annex B: scan NALs
        {
            let mut i = 0;
            while i < data.len() {
                let (prefix, nal_start) =
                    if i + 4 <= data.len() && data[i..].starts_with(&[0, 0, 0, 1]) {
                        (4usize, i + 4)
                    } else if i + 3 <= data.len() && data[i..].starts_with(&[0, 0, 1]) {
                        (3usize, i + 3)
                    } else {
                        i += 1;
                        continue;
                    };
                // find next start code
                let mut j = nal_start;
                while j < data.len() {
                    if j + 3 <= data.len()
                        && (data[j..].starts_with(&[0, 0, 0, 1])
                            || data[j..].starts_with(&[0, 0, 1]))
                    {
                        break;
                    }
                    j += 1;
                }
                if nal_start < data.len() {
                    let nal_type = data[nal_start] & 0x1f;
                    if nal_type == 5 {
                        return true;
                    }
                    if nal_type == 1 || nal_type == 2 || nal_type == 3 || nal_type == 4 {
                        return false;
                    }
                }
                i = j;
                let _ = prefix;
            }
        }
        // AVCC: length-prefixed NALs
        {
            let mut i = 0;
            while i + 4 <= data.len() {
                let len =
                    u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
                if len == 0 || i + 4 + len > data.len() {
                    break;
                }
                let nal_type = data[i + 4] & 0x1f;
                if nal_type == 5 {
                    return true;
                }
                if nal_type == 1 || nal_type == 2 || nal_type == 3 || nal_type == 4 {
                    return false;
                }
                i += 4 + len;
            }
        }
        return false;
    }
    false
}

/// Split one kept range into copy and re-encode parts.
///
/// A part is copyable only when it starts and ends on keyframes.  The GOP that
/// contains `start` (if `start` is not a keyframe) is re-encoded from `start`
/// to the next keyframe; the GOP that contains `end` is re-encoded from the
/// last keyframe before `end` to `end`.  Everything in between is copied.
pub(crate) fn split_segment(index: &KeyframeIndex, start: f64, end: f64) -> Vec<Part> {
    if !(end > start) {
        return Vec::new();
    }

    let reencode_all = vec![Part {
        kind: PartKind::Reencode,
        start,
        end,
    }];

    let tb = frameforge_core_types::Timebase::new(1, 1_000_000);
    let ts = |s: f64| frameforge_core_types::Timestamp::from_seconds(s, tb);
    let start_ts = ts(start);
    let end_ts = ts(end);

    let start_kf = index.latest_keyframe_at_or_before(&start_ts);
    let end_kf = index.first_keyframe_at_or_after(&end_ts);

    let start_is_kf = start_kf
        .map(|e| (e.pts.to_seconds() - start).abs() < ALIGN_EPS)
        .unwrap_or(false);
    let end_is_kf = end_kf
        .map(|e| (e.pts.to_seconds() - end).abs() < ALIGN_EPS)
        .unwrap_or(false);

    // First keyframe strictly after `start`; if `start` itself is not a
    // keyframe, the GOP that contains it ends here.
    let next_kf = index
        .first_keyframe_after(&start_ts)
        .map(|e| e.pts.to_seconds())
        .or_else(|| {
            index
                .first_keyframe_at_or_after(&start_ts)
                .map(|e| e.pts.to_seconds())
                .filter(|t| *t > start + ALIGN_EPS)
        });
    // Last keyframe strictly before `end`; if `end` is past the final keyframe
    // (normal end-of-file), the tail GOP runs from here to `end`.
    let last_kf = index
        .last_keyframe_before(&end_ts)
        .map(|e| e.pts.to_seconds());

    let mut parts = Vec::new();
    let mut cursor = start;

    if !start_is_kf {
        let re_end = next_kf.unwrap_or(end).min(end);
        if re_end > start + EPS {
            parts.push(Part {
                kind: PartKind::Reencode,
                start,
                end: re_end,
            });
            cursor = re_end;
        }
    }

    if end_is_kf {
        if end > cursor + EPS {
            parts.push(Part {
                kind: PartKind::Copy,
                start: cursor,
                end,
            });
        }
    } else {
        let copy_end = last_kf.unwrap_or(cursor).max(cursor).min(end);
        if copy_end > cursor + EPS {
            parts.push(Part {
                kind: PartKind::Copy,
                start: cursor,
                end: copy_end,
            });
            cursor = copy_end;
        }
        if end > cursor + EPS {
            parts.push(Part {
                kind: PartKind::Reencode,
                start: cursor,
                end,
            });
        }
    }

    // Merge adjacent parts of the same kind (happens for very short ranges).
    let mut merged: Vec<Part> = Vec::new();
    for part in parts {
        if let Some(last) = merged.last_mut() {
            if last.kind == part.kind && (part.start - last.end).abs() < ALIGN_EPS {
                last.end = part.end;
                continue;
            }
        }
        merged.push(part);
    }

    if merged.is_empty() {
        reencode_all
    } else {
        merged
    }
}

/// Smart-rendering executor: copy GOPs + re-encode boundary GOPs.
pub struct SmartRenderExecutor {
    clips: Vec<Clip>,
    destination: PathBuf,
    encoder_pref: String,
    quality_preset: String,
    target_width: Option<u32>,
    target_height: Option<u32>,
    target_fps_num: Option<u32>,
    target_fps_den: Option<u32>,
    target_pix_fmt: Option<String>,
    audio_sample_rate: Option<u32>,
    audio_channels: Option<u32>,
    /// Re-encode every part instead of copying GOPs.  Used as a correct
    /// fallback when the source/encoder parameter sets differ or when the
    /// caller explicitly asked for a re-encode.
    force_reencode: bool,
    /// Allow avc3 (in-band SPS/PPS) sample entry for MP4 when the encoder's
    /// parameter sets differ from the source's.
    allow_avc3: bool,
}

impl SmartRenderExecutor {
    pub fn new(
        clips: Vec<Clip>,
        destination: PathBuf,
        encoder_pref: String,
        quality_preset: String,
    ) -> Self {
        Self {
            clips,
            destination,
            encoder_pref,
            quality_preset,
            target_width: None,
            target_height: None,
            target_fps_num: None,
            target_fps_den: None,
            target_pix_fmt: None,
            audio_sample_rate: None,
            audio_channels: None,
            force_reencode: false,
            allow_avc3: true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_normalization(
        mut self,
        width: Option<u32>,
        height: Option<u32>,
        fps_num: Option<u32>,
        fps_den: Option<u32>,
        pix_fmt: Option<String>,
        audio_sample_rate: Option<u32>,
        audio_channels: Option<u32>,
    ) -> Self {
        self.target_width = width;
        self.target_height = height;
        self.target_fps_num = fps_num;
        self.target_fps_den = fps_den;
        self.target_pix_fmt = pix_fmt;
        self.audio_sample_rate = audio_sample_rate;
        self.audio_channels = audio_channels;
        self
    }

    pub fn with_force_reencode(mut self, force: bool) -> Self {
        self.force_reencode = force;
        self
    }

    pub fn with_avc3(mut self, allow: bool) -> Self {
        self.allow_avc3 = allow;
        self
    }

    fn build_clip_map(&self) -> HashMap<SourceId, PathBuf> {
        self.clips.iter().map(|c| (c.id, c.path.clone())).collect()
    }
}

impl ExportExecutor for SmartRenderExecutor {
    fn run(
        &mut self,
        plan: &ExportPlan,
        progress: &ProgressCallback,
        cancel: &CancelToken,
    ) -> Result<()> {
        ffmpeg_next::init()
            .map_err(|e| FrameForgeError::Export(format!("ffmpeg init failed: {e}")))?;
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Fatal);

        if plan.segments.is_empty() {
            return Err(FrameForgeError::Export("no segments in plan".into()));
        }

        let clip_map = self.build_clip_map();
        let estimate = crate::estimate_output_bytes(plan, &clip_map)?;
        crate::ensure_disk_space(&self.destination, estimate)?;
        let output_format = output_format_from_extension(&self.destination);
        let destination = self.destination.clone();
        let encoder_pref = self.encoder_pref.clone();
        let quality_preset = self.quality_preset.clone();
        let width = self.target_width;
        let height = self.target_height;
        let fps_num = self.target_fps_num;
        let fps_den = self.target_fps_den;
        let pix_fmt = self.target_pix_fmt.clone();
        let audio_sample_rate = self.audio_sample_rate;
        let audio_channels = self.audio_channels;

        atomic_write(&destination, |temp_path, fmt| {
            smart_render_multiple_files(
                temp_path,
                plan,
                cancel,
                progress,
                fmt.or(output_format),
                &clip_map,
                &encoder_pref,
                &quality_preset,
                width,
                height,
                fps_num,
                fps_den,
                pix_fmt.as_deref(),
                audio_sample_rate,
                audio_channels,
                self.force_reencode,
                self.allow_avc3,
            )
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn smart_render_multiple_files(
    output_path: &Path,
    plan: &ExportPlan,
    cancel: &CancelToken,
    progress: &ProgressCallback,
    output_format: Option<&str>,
    clip_map: &HashMap<SourceId, PathBuf>,
    encoder_pref: &str,
    quality_preset: &str,
    target_width: Option<u32>,
    target_height: Option<u32>,
    target_fps_num: Option<u32>,
    target_fps_den: Option<u32>,
    target_pix_fmt: Option<&str>,
    audio_sample_rate: Option<u32>,
    audio_channels: Option<u32>,
    mut force_reencode: bool,
    allow_avc3: bool,
) -> Result<()> {
    let fmt = output_format.unwrap_or("mp4");
    let is_mp4 = fmt == "mp4" || fmt == "mov";
    let is_mkv = fmt == "matroska";
    let is_ts = fmt == "mpegts";
    if !force_reencode && !is_mp4 && !is_mkv && !is_ts {
        return Err(FrameForgeError::SmartRenderUnsupported(format!(
            "smart copy requires MP4/MOV/MKV/TS (got '{fmt}')"
        )));
    }
    // Smart copy is rendered into Matroska and then remuxed (stream copy) into
    // MP4.  The intermediate forces every demuxer packet into a refcounted
    // buffer; the final remux is a pure stream copy, so no video is re-encoded.
    // Render smart copy into Matroska first when the final container is
    // MP4/MOV.  Matroska tolerates the negative CTS offsets produced by
    // B-frame GOPs; the final remux (stream copy) applies the MP4-specific
    // movflags.  Writing B-frame packets straight to the MP4 muxer fails
    // with EINVAL.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let (render_path, render_fmt) = if is_mp4 {
        let intermediate = output_path.with_file_name(format!(
            ".{}.{}.tmp.mkv",
            output_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("out"),
            unique
        ));
        (intermediate, "matroska")
    } else {
        (output_path.to_path_buf(), fmt)
    };
    let render_is_mp4 = is_mp4;
    let render_is_mp4_container = render_fmt == "mp4" || render_fmt == "mov";

    let first_source = plan
        .segments
        .first()
        .map(|s| s.source_id)
        .ok_or_else(|| FrameForgeError::Export("no segments in plan".into()))?;
    let first_clip_path = clip_map
        .get(&first_source)
        .ok_or_else(|| FrameForgeError::Export("no clips in map".into()))?;

    let first_ictx = format::input(first_clip_path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;
    let input_streams: Vec<ffmpeg_next::Stream> = first_ictx.streams().collect();

    let video_streams: Vec<&ffmpeg_next::Stream> = input_streams
        .iter()
        .filter(|s| s.parameters().medium() == media::Type::Video)
        .collect();
    if video_streams.len() != 1 {
        return Err(FrameForgeError::SmartRenderUnsupported(format!(
            "expected exactly one video stream, found {}",
            video_streams.len()
        )));
    }
    let first_video = video_streams[0];
    if first_video.parameters().id() != codec::Id::H264 {
        return Err(FrameForgeError::SmartRenderUnsupported(format!(
            "smart rendering requires an H.264 source, found {:?}",
            first_video.parameters().id()
        )));
    }
    if input_streams
        .iter()
        .any(|s| s.parameters().medium() == media::Type::Subtitle)
    {
        return Err(FrameForgeError::SmartRenderUnsupported(
            "subtitle streams are not supported by smart rendering".into(),
        ));
    }

    let video_ist_index = first_video.index();
    let video_ist_tb = first_video.time_base();
    // Probe decoder for pixel format / geometry when the caller did not provide
    // explicit normalization targets.
    let probe_dec_ctx = codec::Context::from_parameters(first_video.parameters())
        .map_err(|e| FrameForgeError::Export(format!("decoder context failed: {e}")))?;
    let mut probe_dec = probe_dec_ctx
        .decoder()
        .video()
        .map_err(|e| FrameForgeError::Export(format!("decoder video failed: {e}")))?;
    probe_dec.set_parameters(first_video.parameters()).ok();
    let src_width = probe_dec.width();
    let src_height = probe_dec.height();
    let src_pix_fmt = probe_dec.format();

    let width = target_width.unwrap_or(src_width);
    let height = target_height.unwrap_or(src_height);
    let frame_rate = match (target_fps_num, target_fps_den) {
        (Some(num), Some(den)) => {
            ffmpeg_next::Rational(num.try_into().unwrap_or(0), den.try_into().unwrap_or(1))
        }
        _ => first_video.rate(),
    };
    let resolved_pix_fmt = target_pix_fmt
        .map(str::to_string)
        .unwrap_or_else(|| format!("{:?}", src_pix_fmt).to_lowercase());
    let sw_format = parse_pixel_format(&resolved_pix_fmt).unwrap_or(src_pix_fmt);

    let mut active_spec = resolve_video_encoder(encoder_pref)?;
    set_last_encoder_report(EncoderReport {
        requested: encoder_pref.to_string(),
        encoder: active_spec.name.clone(),
        hardware: active_spec.hardware,
        fallback_reason: active_spec.fallback_reason.clone(),
    });

    let (src_profile_id, src_level) = unsafe {
        let p = first_video.parameters().as_ptr();
        ((*p).profile, (*p).level)
    };
    let src_profile = profile_name(src_profile_id);
    let src_level = if src_level > 0 {
        Some(src_level as u32)
    } else {
        None
    };
    let src_bframes = if force_reencode {
        None
    } else {
        Some(0) // bframes=0 for boundary re-encode to avoid DTS reordering
    };
    // Extract the source's GOP size (keyframe interval) for the encoder.
    let src_gop = if force_reencode {
        None
    } else {
        build_keyframe_index_cached(first_clip_path, None)
            .ok()
            .and_then(|ki| {
                if ki.entries.len() >= 2 {
                    let d0 = ki.entries[0].pts.to_seconds();
                    let d1 = ki.entries[1].pts.to_seconds();
                    let fps = first_video.rate();
                    let fps_f64 = if fps.denominator() > 0 {
                        fps.numerator() as f64 / fps.denominator() as f64
                    } else {
                        30.0
                    };
                    let gop = ((d1 - d0) * fps_f64).round() as u32;
                    if gop > 0 {
                        Some(gop)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
    };
    let src_refs = {
        let extra = unsafe { parameters_extradata(first_video.parameters().as_ptr()) };
        extra
            .and_then(|e| extract_parameter_sets(&e))
            .and_then(|(_, pps)| pps.first().cloned())
            .and_then(|pps| parse_pps_refs(&pps))
            .unwrap_or(probe_dec.references() as usize)
    };
    let mut encoder_params = VideoEncoderParams {
        width,
        height,
        sw_format,
        frame_rate,
        time_base: video_ist_tb,
        bframes: src_bframes,
        profile: src_profile.clone(),
        level: src_level,
        x264_params: if force_reencode {
            None
        } else {
            let (wp, wb, cabac) = {
                let extra = unsafe { parameters_extradata(first_video.parameters().as_ptr()) };
                match extra {
                    Some(e) => {
                        let sets = extract_parameter_sets(&e);
                        match sets {
                            Some((_sps, pps)) => {
                                if let Some(pps) = pps.first() {
                                    parse_weighted_pred(pps)
                                } else {
                                    (1, 0, true)
                                }
                            }
                            None => (1, 0, true),
                        }
                    }
                    None => (1, 0, true),
                }
            };
            let cabac_flag = if cabac { 1 } else { 0 };
            let mut params = format!(
                "weightp={}:weightb={}:cabac={}:open-gop=0:ref={}",
                wp, wb, cabac_flag, src_refs
            );
            if matches!(
                src_profile.as_deref(),
                Some("high") | Some("high10") | Some("high422") | Some("high444")
            ) {
                params = format!("{}:8x8dct=1", params);
            }
            if is_ts || is_mkv {
                params = format!("{}:repeat-headers=1", params);
            }
            Some(params)
        },
        refs: Some(src_refs),
        gop_size: src_gop,
        global_header: is_mp4,
    };

    // Probe encoder: its extradata decides whether we need `avc3` in-band
    // parameter sets.  Open it once, compare, then drop it; every re-encode
    // part gets a fresh encoder so it starts with an IDR.
    let (probe_encoder, _probe_hw, probe_spec) =
        open_encoder_with_fallback(&active_spec, &encoder_params, quality_preset, encoder_pref)?;
    active_spec = probe_spec;
    let mut compat = true;
    if !force_reencode && is_mp4 {
        let src = unsafe { parameters_extradata(first_video.parameters().as_ptr()) }.ok_or_else(
            || FrameForgeError::SmartRenderUnsupported("source has no H.264 extradata".into()),
        )?;
        let enc = unsafe { codec_extradata(probe_encoder.as_ptr()) }.ok_or_else(|| {
            FrameForgeError::SmartRenderUnsupported("encoder produced no H.264 extradata".into())
        })?;
        let src_sets = extract_parameter_sets(&src).ok_or_else(|| {
            FrameForgeError::SmartRenderUnsupported("source has no H.264 SPS/PPS".into())
        })?;
        let enc_sets = extract_parameter_sets(&enc).ok_or_else(|| {
            FrameForgeError::SmartRenderUnsupported("encoder produced no H.264 SPS/PPS".into())
        })?;
        let src_sps = src_sets.0.first();
        let enc_sps = enc_sets.0.first();
        let src_pps = src_sets.1.first();
        let enc_pps = enc_sets.1.first();
        let core_match = match (src_sps, enc_sps) {
            (Some(a), Some(b)) => {
                let src_info = parse_sps_info(a);
                let enc_info = parse_sps_info(b);
                src_info == enc_info
            }
            _ => false,
        };
        // PPS must match byte-for-byte: the output container carries the
        // source's PPS (avc1), so a re-encoded GOP whose PPS differs would
        // be decoded with the wrong parameter set and corrupt the picture.
        let pps_ok = match (src_pps, enc_pps) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        compat = core_match && pps_ok;
        if !compat {
            if allow_avc3 {
                // Use in-band SPS/PPS (avc3) so copied and re-encoded GOPs
                // can carry different parameter sets without corrupting decode.
            } else {
                eprintln!("SMART COPY: SPS/PPS differ; falling back to full re-encode");
                force_reencode = true;
            }
        }
    } else if !force_reencode {
        // TS/MKV: do not require SPS/PPS equality.
        eprintln!("SMART COPY (ts/mkv): skipping SPS/PPS equality check");
        compat = true;
    }
    let use_avc3 = !force_reencode && render_is_mp4 && !compat && allow_avc3;
    if use_avc3 {
        encoder_params.global_header = false;
    } else if render_is_mp4 {
        encoder_params.global_header = true;
    } else {
        encoder_params.global_header = false;
    }

    let mut octx = format::output_as(&render_path, render_fmt)
        .map_err(|e| FrameForgeError::Export(format!("format::output_as failed: {e}")))?;

    // Video output stream: keep the source codec parameters (so `avc1`/`avc3`
    // has a valid configuration record) and choose the sample entry.
    let mut video_ost = octx
        .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
        .map_err(|e| FrameForgeError::Export(format!("add_stream failed: {e}")))?;
    // The container's `avcC` must describe the COPIED packets (source's
    // SPS/PPS), because the copied GOPs are written as-is.  The re-encoded
    // boundary GOPs must produce compatible SPS/PPS (enforced by the
    // compatibility check above) so they decode with the same avcC.
    if force_reencode {
        video_ost.set_parameters(&probe_encoder);
    } else {
        video_ost.set_parameters(first_video.parameters());
    }
    video_ost.set_time_base(video_ist_tb);
    let video_ost_index = video_ost.index();
    // Use avc3 (in-band parameter sets) for smart copy so that each
    // keyframe carries its own SPS/PPS.  This handles the case where the
    // copied and re-encoded packets have different SPS/PPS.
    unsafe {
        (*video_ost.parameters().as_mut_ptr()).codec_tag = if !render_is_mp4_container {
            0
        } else if force_reencode {
            AVC1_TAG
        } else if use_avc3 {
            AVC3_TAG
        } else {
            AVC1_TAG
        };
    }

    // Audio output streams: always re-encoded with one continuous sample
    // counter.  This is cheap next to video and avoids AAC priming gaps.
    let mut audio_transcoders: Vec<Option<AudioTranscoder>> =
        (0..input_streams.len()).map(|_| None).collect();
    // MP2 audio in .ts is stream-copied instead of re-encoded.
    let mut audio_copy_streams: Vec<(usize, usize)> = Vec::new();
    let mut audio_copy_base_pts: Vec<i64> = Vec::new();
    let mut audio_copy_base_dts: Vec<i64> = Vec::new();
    let mut audio_copy_last_pts: Vec<i64> = Vec::new();
    let mut audio_copy_last_dts: Vec<i64> = Vec::new();

    for (ist_index, ist) in input_streams.iter().enumerate() {
        if ist.parameters().medium() != media::Type::Audio {
            continue;
        }

        // Stream-copy MP2 audio when targeting .ts.
        if is_ts && ist.parameters().id() == codec::Id::MP2 {
            let mut ost = octx
                .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
                .map_err(|e| FrameForgeError::Export(format!("add_stream failed: {e}")))?;
            ost.set_parameters(ist.parameters());
            ost.set_time_base(ist.time_base());
            let ost_index = ost.index();
            audio_copy_streams.push((ist_index, ost_index));
            audio_copy_base_pts.push(0);
            audio_copy_base_dts.push(0);
            audio_copy_last_pts.push(0);
            audio_copy_last_dts.push(0);
            continue;
        }

        let dec_ctx = codec::Context::from_parameters(ist.parameters())
            .map_err(|e| FrameForgeError::Export(format!("audio decoder context failed: {e}")))?;
        let mut dec_audio = dec_ctx
            .decoder()
            .audio()
            .map_err(|e| FrameForgeError::Export(format!("audio decoder failed: {e}")))?;
        dec_audio.set_parameters(ist.parameters()).ok();

        let audio_codec = select_audio_encoder(encoder_pref)?;
        let enc_ctx = codec::Context::new_with_codec(audio_codec);
        let mut enc_audio = enc_ctx
            .encoder()
            .audio()
            .map_err(|e| FrameForgeError::Export(format!("audio encoder failed: {e}")))?;

        let src_rate = unsafe { (*ist.parameters().as_ptr()).sample_rate };
        let src_channels = unsafe { (*ist.parameters().as_ptr()).channels };
        let src_channel_layout = unsafe { (*ist.parameters().as_ptr()).channel_layout };

        let rate = audio_sample_rate.unwrap_or_else(|| {
            if src_rate > 0 {
                src_rate as u32
            } else if dec_audio.rate() > 0 {
                dec_audio.rate()
            } else {
                44100
            }
        });
        let channels = audio_channels.unwrap_or_else(|| {
            if src_channels > 0 {
                src_channels as u32
            } else if dec_audio.channels() > 0 {
                dec_audio.channels() as u32
            } else {
                2
            }
        });
        let layout =
            if src_channel_layout != 0 && src_channels > 0 && src_channels == channels as i32 {
                ffmpeg_next::channel_layout::ChannelLayout::from_bits_truncate(src_channel_layout)
            } else if channels == 1 {
                ffmpeg_next::channel_layout::ChannelLayout::MONO
            } else {
                ffmpeg_next::channel_layout::ChannelLayout::STEREO
            };

        enc_audio.set_rate(rate as i32);
        enc_audio.set_channels(channels as i32);
        enc_audio.set_channel_layout(layout);
        enc_audio.set_time_base(ffmpeg_next::Rational(1, rate as i32));
        enc_audio.set_format(dec_audio.format());

        // Build a resampler when the requested output sample rate or channel
        // count differs from the source, so audio is actually converted.
        let src_rate_eff = if src_rate > 0 {
            src_rate as u32
        } else if dec_audio.rate() > 0 {
            dec_audio.rate()
        } else {
            rate
        };
        let src_channels_eff = if src_channels > 0 {
            src_channels as u32
        } else if dec_audio.channels() > 0 {
            dec_audio.channels() as u32
        } else {
            channels
        };
        let src_layout = if src_channel_layout != 0 {
            ffmpeg_next::channel_layout::ChannelLayout::from_bits_truncate(src_channel_layout)
        } else if src_channels_eff == 1 {
            ffmpeg_next::channel_layout::ChannelLayout::MONO
        } else {
            ffmpeg_next::channel_layout::ChannelLayout::STEREO
        };
        let opened_audio = enc_audio
            .open_with(Dictionary::new())
            .map_err(|e| FrameForgeError::Export(format!("audio encoder open failed: {e}")))?;

        let encoder_format = opened_audio.format();
        if encoder_format == ffmpeg_next::format::Sample::None {
            return Err(FrameForgeError::Export(
                "audio encoder reported an invalid sample format".into(),
            ));
        }
        let frame_size = opened_audio.frame_size() as usize;
        let fifo = if frame_size > 0 {
            Some(AudioFifo::new(
                encoder_format,
                opened_audio.channels() as i32,
            )?)
        } else {
            None
        };

        let needs_resample = src_rate_eff != rate
            || src_channels_eff != channels
            || dec_audio.format() != encoder_format;
        let resampler = if needs_resample {
            Some(
                ffmpeg_next::software::resampling::Context::get(
                    dec_audio.format(),
                    src_layout,
                    src_rate_eff,
                    encoder_format,
                    layout,
                    rate,
                )
                .map_err(|e| FrameForgeError::Export(format!("resampler init failed: {e}")))?,
            )
        } else {
            None
        };

        let mut ost = octx
            .add_stream(audio_codec)
            .map_err(|e| FrameForgeError::Export(format!("add_stream failed: {e}")))?;
        ost.set_parameters(&opened_audio);
        ost.set_time_base(ffmpeg_next::Rational(1, rate as i32));
        let ost_index = ost.index();

        audio_transcoders[ist_index] = Some(AudioTranscoder {
            decoder: dec_audio,
            encoder: opened_audio,
            ost_index,
            time_base: ffmpeg_next::Rational(1, rate as i32),
            next_pts: 0,
            resampler,
            frame_size,
            fifo_format: encoder_format,
            fifo_layout: layout,
            fifo,
        });
    }

    octx.set_metadata(first_ictx.metadata().to_owned());
    if render_fmt == "matroska" {
        let mut opts = Dictionary::new();
        opts.set("cluster_time_limit", "1");
        opts.set("cluster_size_limit", "1000000");
        octx.write_header_with(opts)
            .map_err(|e| FrameForgeError::Export(format!("write_header failed: {e}")))?;
    } else if render_is_mp4 {
        octx.write_header()
            .map_err(|e| FrameForgeError::Export(format!("write_header failed: {e}")))?;
    } else {
        octx.write_header()
            .map_err(|e| FrameForgeError::Export(format!("write_header failed: {e}")))?;
    }

    let total_segments = plan.segments.len() as f32;
    let mut any_reencode = false;
    let mut base_pts: i64 = 0;
    let mut base_dts: i64 = 0;
    let mut last_pts: i64 = i64::MIN;
    let mut last_dts: i64 = i64::MIN;

    for (seg_idx, segment) in plan.segments.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }

        let clip_path = clip_map.get(&segment.source_id).ok_or_else(|| {
            FrameForgeError::Export(format!(
                "clip not found for source_id: {}",
                segment.source_id.0
            ))
        })?;

        let seg_start = segment.range.start.to_seconds();
        let seg_end = segment.range.end.to_seconds();
        let start_offset_seconds = {
            let ictx = format::input(clip_path)
                .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;
            start_offset(&ictx, video_ist_index)
        };
        let parts = if force_reencode {
            vec![Part {
                kind: PartKind::Reencode,
                start: seg_start,
                end: seg_end,
            }]
        } else {
            let keyframe_index = build_keyframe_index_cached(clip_path, Some(seg_end))?;
            let mut parts = split_segment(&keyframe_index, seg_start, seg_end);
            // Open GOP safety: if a Copy part's first packet is not a real IDR,
            // re-encode that part instead.
            let mut ictx = format::input(clip_path)
                .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;
            for part in parts.iter_mut() {
                if part.kind == PartKind::Copy {
                    let idr = first_packet_is_idr(
                        &mut ictx,
                        video_ist_index,
                        start_offset_seconds,
                        part.start,
                    );
                    if !idr {
                        part.kind = PartKind::Reencode;
                    }
                }
            }
            // Ratio rule: smart cut only when re-encoded duration < 25% of segment.
            let seg_duration = (seg_end - seg_start).max(0.0);
            let reenc_duration: f64 = parts
                .iter()
                .filter(|p| p.kind == PartKind::Reencode)
                .map(|p| p.duration())
                .sum();
            if seg_duration > 0.0 && reenc_duration / seg_duration >= 0.25 {
                vec![Part {
                    kind: PartKind::Reencode,
                    start: seg_start,
                    end: seg_end,
                }]
            } else {
                parts
            }
        };

        let mut ictx = format::input(clip_path)
            .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

        let seek_target_seconds = start_offset_seconds + seg_start;
        let seek_pts = (seek_target_seconds * 1_000_000.0) as i64;
        ictx.seek(seek_pts, i64::MIN..seek_pts)
            .map_err(|e| FrameForgeError::Export(format!("seek failed: {e}")))?;

        for (part_idx, part) in parts.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(FrameForgeError::Export("export cancelled".into()));
            }

            match part.kind {
                PartKind::Copy => {
                    let saved = (base_pts, base_dts, last_pts, last_dts);
                    if let Err(e) = copy_part(
                        &mut ictx,
                        &mut octx,
                        video_ist_index,
                        video_ost_index,
                        start_offset_seconds,
                        *part,
                        &mut base_pts,
                        &mut base_dts,
                        &mut last_pts,
                        &mut last_dts,
                        cancel,
                    ) {
                        eprintln!("copy_part failed ({e}); falling back to re-encode for this GOP");
                        base_pts = saved.0;
                        base_dts = saved.1;
                        last_pts = saved.2;
                        last_dts = saved.3;
                        any_reencode = true;
                        reencode_part(
                            &mut ictx,
                            &mut octx,
                            video_ist_index,
                            video_ost_index,
                            start_offset_seconds,
                            *part,
                            &mut active_spec,
                            &encoder_params,
                            quality_preset,
                            &mut base_pts,
                            &mut base_dts,
                            &mut last_pts,
                            &mut last_dts,
                            cancel,
                        )?;
                    }
                }
                PartKind::Reencode => {
                    any_reencode = true;
                    reencode_part(
                        &mut ictx,
                        &mut octx,
                        video_ist_index,
                        video_ost_index,
                        start_offset_seconds,
                        *part,
                        &mut active_spec,
                        &encoder_params,
                        quality_preset,
                        &mut base_pts,
                        &mut base_dts,
                        &mut last_pts,
                        &mut last_dts,
                        cancel,
                    )?
                }
            }

            let part_fraction = (part_idx + 1) as f32 / parts.len().max(1) as f32;
            progress((seg_idx as f32 + part_fraction) / total_segments);
        }

        // Copy MP2 audio streams (when targeting .ts).
        for k in 0..audio_copy_streams.len() {
            let (ist_idx, ost_idx) = audio_copy_streams[k];
            if let Err(e) = copy_audio_stream(
                &mut ictx,
                &mut octx,
                ist_idx,
                ost_idx,
                start_offset_seconds,
                seg_start,
                seg_end,
                &mut audio_copy_base_pts[k],
                &mut audio_copy_base_dts[k],
                &mut audio_copy_last_pts[k],
                &mut audio_copy_last_dts[k],
                cancel,
            ) {
                eprintln!("audio copy failed ({e}); continuing");
            }
        }

        // Audio for the whole kept range of this segment.
        process_audio(
            &mut ictx,
            &mut octx,
            start_offset_seconds,
            seg_start,
            seg_end,
            &mut audio_transcoders,
            cancel,
        )?;

        progress((seg_idx + 1) as f32 / total_segments);
    }

    if !force_reencode {
        let label = if any_reencode {
            format!("smart copy ({} boundary GOPs)", active_spec.name)
        } else {
            "smart copy (all GOPs copied)".to_string()
        };
        set_last_encoder_report(EncoderReport {
            requested: encoder_pref.to_string(),
            encoder: label,
            hardware: active_spec.hardware,
            fallback_reason: active_spec.fallback_reason.clone(),
        });
    }

    // Flush audio encoders.  Flush packets continue after the last sample
    // counter; the encoder's own PTS may be in a different timebase and must
    // be rebased, otherwise the container duration explodes.
    for t in audio_transcoders.iter_mut().flatten() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }
        // Send any samples left in the FIFO (padded to the encoder's frame
        // size) before signaling EOF, otherwise the tail is silently lost.
        flush_audio_fifo(t, &mut octx)?;
        t.encoder
            .send_eof()
            .map_err(|e| FrameForgeError::Export(format!("audio send_eof failed: {e}")))?;
        let out_tb = octx.stream(t.ost_index).unwrap().time_base();
        let mut out_pkt = packet::Packet::empty();
        while t.encoder.receive_packet(&mut out_pkt).is_ok() {
            out_pkt.rescale_ts(t.time_base, out_tb);
            if out_pkt.pts().is_none() {
                out_pkt.set_pts(Some(t.next_pts));
            }
            if out_pkt.dts().is_none() {
                out_pkt.set_dts(out_pkt.pts());
            }
            out_pkt.set_stream(t.ost_index);
            out_pkt.set_position(-1);
            out_pkt
                .write_interleaved(&mut octx)
                .map_err(|e| FrameForgeError::Export(format!("audio flush write failed: {e}")))?;
        }
    }

    octx.write_trailer()
        .map_err(|e| FrameForgeError::Export(format!("write_trailer failed: {e}")))?;
    drop(octx);

    if render_path != output_path {
        remux_copy(&render_path, output_path, fmt, Some(video_ist_tb.1 as i64))?;
        std::fs::remove_file(&render_path).ok();
    }

    progress(1.0);
    Ok(())
}

/// Stream-copy the Matroska intermediate into the final MP4/MOV.
///
/// The system `ffmpeg` CLI is used for this step because its MP4 muxer handles
/// the negative CTS offsets produced by B-frames correctly and preserves every
/// copied packet.  If the CLI is unavailable, the caller falls back to a full
/// re-encode.
fn remux_copy(input: &Path, output: &Path, fmt: &str, timescale: Option<i64>) -> Result<()> {
    let out_fmt = if fmt == "mov" { "mov" } else { "mp4" };
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-c")
        .arg("copy")
        .arg("-use_editlist")
        .arg("0")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-fflags")
        .arg("+genpts")
        .arg("-avoid_negative_ts")
        .arg("make_zero");
    if let Some(ts) = timescale {
        cmd.arg("-video_track_timescale").arg(ts.to_string());
    }
    cmd.arg("-f").arg(out_fmt).arg(output);
    let status = cmd.status().map_err(|e| {
        FrameForgeError::SmartRenderUnsupported(format!(
            "ffmpeg CLI is required for the smart-render remux: {e}"
        ))
    })?;
    if !status.success() {
        return Err(FrameForgeError::SmartRenderUnsupported(format!(
            "ffmpeg remux failed with status {status}"
        )));
    }
    Ok(())
}

fn start_offset(ictx: &format::context::Input, video_ist_index: usize) -> f64 {
    ictx.stream(video_ist_index)
        .map(|s| {
            let st = s.start_time();
            if st == i64::MIN {
                0.0
            } else {
                st as f64 * (s.time_base().0 as f64 / s.time_base().1 as f64)
            }
        })
        .unwrap_or(0.0)
}

fn ensure_pts_gte_dts(pkt: &mut packet::Packet) {
    if let (Some(pts), Some(dts)) = (pkt.pts(), pkt.dts()) {
        if pts < dts {
            pkt.set_pts(Some(dts));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_part(
    ictx: &mut format::context::Input,
    octx: &mut format::context::Output,
    video_ist_index: usize,
    video_ost_index: usize,
    start_offset_seconds: f64,
    part: Part,
    base_pts: &mut i64,
    base_dts: &mut i64,
    last_pts: &mut i64,
    last_dts: &mut i64,
    cancel: &CancelToken,
) -> Result<()> {
    let ist_tb = ictx
        .stream(video_ist_index)
        .ok_or_else(|| FrameForgeError::Export("video stream missing".into()))?
        .time_base();
    let out_tb = octx
        .stream(video_ost_index)
        .ok_or_else(|| FrameForgeError::Export("video output stream missing".into()))?
        .time_base();
    let tb_sec = ist_tb.0 as f64 / ist_tb.1 as f64;

    let seek_target_seconds = start_offset_seconds + part.start;
    let seek_pts = (seek_target_seconds * 1_000_000.0) as i64;
    ictx.seek(seek_pts, i64::MIN..seek_pts)
        .map_err(|e| FrameForgeError::Export(format!("seek failed: {e}")))?;

    let mut stream_start_pts: Option<i64> = None;
    let mut stream_start_dts: Option<i64> = None;
    let mut wrote = false;
    let mut part_last_pts = *base_pts;
    let mut part_last_dts = *base_dts;

    for (stream, mut pkt) in ictx.packets() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }
        if stream.index() != video_ist_index {
            continue;
        }

        let pkt_pts = pkt.pts().unwrap_or(0);
        let pkt_seconds = pkt_pts as f64 * tb_sec - start_offset_seconds;
        if pkt_seconds < part.start - EPS {
            continue;
        }
        if pkt_seconds + EPS >= part.end {
            break;
        }

        let origin = pkt.dts().unwrap_or(pkt_pts);
        if stream_start_pts.is_none() {
            stream_start_pts = Some(origin);
            stream_start_dts = Some(origin);
        }
        let stream_origin = stream_start_dts.unwrap_or(origin);
        let relative_pts = pkt_pts - stream_origin;
        let relative_dts = pkt.dts().unwrap_or(pkt_pts) - stream_origin;

        pkt.set_pts(Some(relative_pts));
        pkt.set_dts(Some(relative_dts));
        pkt.rescale_ts(ist_tb, out_tb);

        let mut new_pts = pkt.pts().unwrap_or(0) + *base_pts;
        let mut new_dts = pkt.dts().unwrap_or(0) + *base_dts;

        if new_dts < *base_dts {
            new_dts = *base_dts;
        }
        // Matroska (and the intermediate we render to) requires PTS >= DTS;
        // clamp the decode timestamp down to the presentation timestamp for
        // B-frames instead of dropping the GOP.
        if new_dts > new_pts {
            new_dts = new_pts;
        }
        // Keep DTS non-decreasing within the part without pushing it past PTS.
        if new_dts < *last_dts {
            new_dts = *last_dts;
            if new_dts > new_pts {
                new_pts = new_dts;
            }
        }
        if new_pts < *base_pts {
            new_pts = *base_pts;
            if new_pts < new_dts {
                new_pts = new_dts;
            }
        }

        pkt.set_pts(Some(new_pts));
        pkt.set_dts(Some(new_dts));
        pkt.set_stream(video_ost_index);
        pkt.set_position(-1);
        if let Err(e) = pkt.write_interleaved(octx) {
            return Err(FrameForgeError::Export(format!(
                "video copy write failed: {e}"
            )));
        }
        part_last_pts = new_pts;
        part_last_dts = new_dts;
        *last_pts = new_pts;
        *last_dts = new_dts;
        wrote = true;
    }

    advance_timeline(
        part,
        out_tb,
        base_pts,
        base_dts,
        part_last_pts,
        part_last_dts,
        wrote,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_audio_stream(
    ictx: &mut format::context::Input,
    octx: &mut format::context::Output,
    audio_ist_index: usize,
    audio_ost_index: usize,
    video_start_offset_seconds: f64,
    seg_start: f64,
    seg_end: f64,
    base_pts: &mut i64,
    base_dts: &mut i64,
    last_pts: &mut i64,
    last_dts: &mut i64,
    cancel: &CancelToken,
) -> Result<()> {
    let ist_tb = ictx
        .stream(audio_ist_index)
        .ok_or_else(|| FrameForgeError::Export("audio stream missing".into()))?
        .time_base();
    let out_tb = octx
        .stream(audio_ost_index)
        .ok_or_else(|| FrameForgeError::Export("audio output stream missing".into()))?
        .time_base();
    let tb_sec = ist_tb.0 as f64 / ist_tb.1 as f64;

    let stream_start_offset = ictx
        .stream(audio_ist_index)
        .map(|s| {
            let st = s.start_time();
            if st == i64::MIN {
                0.0
            } else {
                st as f64 * tb_sec
            }
        })
        .unwrap_or(0.0);

    let seek_target_seconds = stream_start_offset + seg_start;
    let seek_pts = (seek_target_seconds * 1_000_000.0) as i64;
    ictx.seek(seek_pts, i64::MIN..seek_pts).ok();

    let mut stream_start_pts: Option<i64> = None;
    let mut stream_start_dts: Option<i64> = None;
    let mut wrote = false;
    let mut part_last_pts = *base_pts;
    let mut part_last_dts = *base_dts;

    for (stream, mut pkt) in ictx.packets() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }
        if stream.index() != audio_ist_index {
            continue;
        }

        let pkt_pts = pkt.pts().unwrap_or(0);
        let pkt_seconds = pkt_pts as f64 * tb_sec - stream_start_offset;
        if pkt_seconds < seg_start - EPS {
            continue;
        }
        if pkt_seconds + EPS >= seg_end {
            break;
        }

        let origin = pkt.dts().unwrap_or(pkt_pts);
        if stream_start_pts.is_none() {
            stream_start_pts = Some(origin);
            stream_start_dts = Some(origin);
        }
        let stream_origin = stream_start_dts.unwrap_or(origin);
        let relative_pts = pkt_pts - stream_origin;
        let relative_dts = pkt.dts().unwrap_or(pkt_pts) - stream_origin;

        pkt.set_pts(Some(relative_pts));
        pkt.set_dts(Some(relative_dts));
        pkt.rescale_ts(ist_tb, out_tb);

        let mut new_pts = pkt.pts().unwrap_or(0) + *base_pts;
        let mut new_dts = pkt.dts().unwrap_or(0) + *base_dts;

        if new_dts < *base_dts {
            new_dts = *base_dts;
        }
        if new_dts <= *last_dts {
            new_dts = *last_dts + 1;
        }
        if new_pts < new_dts {
            new_pts = new_dts;
        }
        if new_pts < *base_pts {
            new_pts = *base_pts;
            if new_pts < new_dts {
                new_pts = new_dts;
            }
        }

        let data = pkt.data().unwrap_or(&[]);
        let mut out_pkt = packet::Packet::copy(data);
        out_pkt.set_flags(pkt.flags());
        out_pkt.set_pts(Some(new_pts));
        out_pkt.set_dts(Some(new_dts));
        out_pkt.set_stream(audio_ost_index);
        out_pkt.set_position(-1);

        out_pkt
            .write_interleaved(octx)
            .map_err(|e| FrameForgeError::Export(format!("audio copy write failed: {e}")))?;
        part_last_pts = new_pts;
        part_last_dts = new_dts;
        *last_pts = new_pts;
        *last_dts = new_dts;
        wrote = true;
    }

    if wrote {
        *base_pts = part_last_pts + 1;
        *base_dts = part_last_dts + 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn reencode_part(
    ictx: &mut format::context::Input,
    octx: &mut format::context::Output,
    video_ist_index: usize,
    video_ost_index: usize,
    start_offset_seconds: f64,
    part: Part,
    active_spec: &mut VideoEncoderSpec,
    encoder_params: &VideoEncoderParams,
    quality_preset: &str,
    base_pts: &mut i64,
    base_dts: &mut i64,
    last_pts: &mut i64,
    last_dts: &mut i64,
    cancel: &CancelToken,
) -> Result<()> {
    let ist_tb = ictx
        .stream(video_ist_index)
        .ok_or_else(|| FrameForgeError::Export("video stream missing".into()))?
        .time_base();
    let out_tb = octx
        .stream(video_ost_index)
        .ok_or_else(|| FrameForgeError::Export("video output stream missing".into()))?
        .time_base();
    let tb_sec = ist_tb.0 as f64 / ist_tb.1 as f64;

    let params = ictx
        .stream(video_ist_index)
        .ok_or_else(|| FrameForgeError::Export("video stream missing".into()))?
        .parameters();
    let dec_ctx = codec::Context::from_parameters(params.clone())
        .map_err(|e| FrameForgeError::Export(format!("decoder context failed: {e}")))?;
    let mut dec = dec_ctx
        .decoder()
        .video()
        .map_err(|e| FrameForgeError::Export(format!("decoder video failed: {e}")))?;
    dec.set_parameters(params).ok();

    // A fresh encoder per part guarantees the first frame is an IDR.
    let (mut enc, hw, spec) = open_encoder_with_fallback(
        active_spec,
        encoder_params,
        quality_preset,
        &active_spec.name,
    )?;
    *active_spec = spec;
    let seek_target_seconds = start_offset_seconds + part.start;
    let seek_pts = (seek_target_seconds * 1_000_000.0) as i64;
    ictx.seek(seek_pts, i64::MIN..seek_pts)
        .map_err(|e| FrameForgeError::Export(format!("seek failed: {e}")))?;
    dec.flush();

    let mut frame = ffmpeg_next::util::frame::Video::empty();
    let frame_step = if encoder_params.frame_rate.numerator() > 0
        && encoder_params.frame_rate.denominator() > 0
        && encoder_params.time_base.numerator() > 0
    {
        (encoder_params.time_base.denominator() as i64
            * encoder_params.frame_rate.denominator() as i64)
            / (encoder_params.time_base.numerator() as i64
                * encoder_params.frame_rate.numerator() as i64)
    } else {
        1
    };
    let mut local_frame_pts: i64 = 0;
    let mut wrote = false;
    let mut part_last_pts = *base_pts;
    let mut part_last_dts = *base_dts;

    // Feed packets a little past the end so B-frames at the boundary have their
    // future references.  Frames outside the part are discarded below.
    let packet_cutoff = part.end + 1.0;

    for (stream, pkt) in ictx.packets() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }
        if stream.index() != video_ist_index {
            continue;
        }
        let pkt_seconds = pkt.pts().unwrap_or(0) as f64 * tb_sec - start_offset_seconds;
        if pkt_seconds >= packet_cutoff {
            break;
        }

        dec.send_packet(&pkt).ok();
        while dec.receive_frame(&mut frame).is_ok() {
            let frame_seconds = frame.pts().unwrap_or(0) as f64 * tb_sec - start_offset_seconds;
            if frame_seconds < part.start - EPS || frame_seconds + EPS >= part.end {
                continue;
            }

            let mut norm = normalize_video_frame(
                &frame,
                Some(encoder_params.width),
                Some(encoder_params.height),
                Some(&pixel_format_name(encoder_params.sw_format)),
            )?;
            // The encoder must see a 0-based presentation timeline for this
            // part; otherwise its output PTS includes the source timestamp and
            // the output timeline gets a gap at every join.
            norm.set_pts(Some(local_frame_pts));
            local_frame_pts += frame_step.max(1);

            if let Some(hw) = hw.as_ref() {
                unsafe { hw.upload(&mut enc, &norm) }
                    .map_err(|e| FrameForgeError::Export(format!("VAAPI upload failed: {e}")))?;
            } else {
                enc.send_frame(&norm).ok();
            }

            let mut out_pkt = packet::Packet::empty();
            while enc.receive_packet(&mut out_pkt).is_ok() {
                out_pkt.rescale_ts(ist_tb, out_tb);
                let enc_pts = out_pkt.pts().unwrap_or(0);
                let enc_dts = out_pkt.dts().unwrap_or(enc_pts);
                let mut new_pts = *base_pts + enc_pts;
                let mut new_dts = *base_dts + enc_dts;

                if new_dts < *base_dts {
                    new_dts = *base_dts;
                }
                if new_dts <= *last_dts {
                    new_dts = *last_dts + 1;
                }
                if new_pts < new_dts {
                    new_pts = new_dts;
                }
                if new_pts < *base_pts {
                    new_pts = *base_pts;
                    if new_pts < new_dts {
                        new_pts = new_dts;
                    }
                }

                out_pkt.set_pts(Some(new_pts));
                out_pkt.set_dts(Some(new_dts));
                out_pkt.set_stream(video_ost_index);
                out_pkt.set_position(-1);
                ensure_pts_gte_dts(&mut out_pkt);

                let ost_extra = octx.stream(video_ost_index).map(|s| s.parameters());
                let ost_extra = octx.stream(video_ost_index).map(|s| s.parameters());
                let avcc_mode = ost_extra
                    .as_ref()
                    .map(|p| {
                        let e = unsafe { parameters_extradata(p.as_ptr()) };
                        e.map(|d| !d.is_empty() && d[0] == 0x01).unwrap_or(false)
                    })
                    .unwrap_or(false);
                if avcc_mode {
                    let converted = annexb_to_avcc(out_pkt.data().unwrap_or(&[]));
                    let mut write_pkt = packet::Packet::copy(&converted);
                    write_pkt.set_pts(out_pkt.pts());
                    write_pkt.set_dts(out_pkt.dts());
                    write_pkt.set_flags(out_pkt.flags());
                    write_pkt.set_stream(video_ost_index);
                    write_pkt.set_position(-1);
                    write_pkt.write_interleaved(octx).map_err(|e| {
                        FrameForgeError::Export(format!("video encode write failed: {e}"))
                    })?;
                } else {
                    out_pkt.write_interleaved(octx).map_err(|e| {
                        FrameForgeError::Export(format!("video encode write failed: {e}"))
                    })?;
                }
                part_last_pts = new_pts;
                part_last_dts = new_dts;
                *last_pts = new_pts;
                *last_dts = new_dts;
                wrote = true;
            }
        }
    }

    // Flush this part's encoder.
    enc.send_eof().ok();
    let mut out_pkt = packet::Packet::empty();
    while enc.receive_packet(&mut out_pkt).is_ok() {
        out_pkt.rescale_ts(ist_tb, out_tb);
        let enc_pts = out_pkt.pts().unwrap_or(0);
        let enc_dts = out_pkt.dts().unwrap_or(enc_pts);
        let mut new_pts = *base_pts + enc_pts;
        let mut new_dts = *base_dts + enc_dts;
        if new_dts < *base_dts {
            new_dts = *base_dts;
        }
        if new_dts <= *last_dts {
            new_dts = *last_dts + 1;
        }
        if new_pts < new_dts {
            new_pts = new_dts;
        }
        if new_pts < *base_pts {
            new_pts = *base_pts;
            if new_pts < new_dts {
                new_pts = new_dts;
            }
        }
        out_pkt.set_pts(Some(new_pts));
        out_pkt.set_dts(Some(new_dts));
        out_pkt.set_stream(video_ost_index);
        out_pkt.set_position(-1);
        ensure_pts_gte_dts(&mut out_pkt);
        let ost_extra = octx.stream(video_ost_index).map(|s| s.parameters());
        let ost_extra = octx.stream(video_ost_index).map(|s| s.parameters());
        let avcc_mode = ost_extra
            .as_ref()
            .map(|p| {
                let e = unsafe { parameters_extradata(p.as_ptr()) };
                e.map(|d| !d.is_empty() && d[0] == 0x01).unwrap_or(false)
            })
            .unwrap_or(false);
        if avcc_mode {
            let converted = annexb_to_avcc(out_pkt.data().unwrap_or(&[]));
            let mut write_pkt = packet::Packet::copy(&converted);
            write_pkt.set_pts(out_pkt.pts());
            write_pkt.set_dts(out_pkt.dts());
            write_pkt.set_flags(out_pkt.flags());
            write_pkt.set_stream(video_ost_index);
            write_pkt.set_position(-1);
            write_pkt.write_interleaved(octx).map_err(|e| {
                FrameForgeError::Export(format!("video encode flush write failed: {e}"))
            })?;
        } else {
            out_pkt.write_interleaved(octx).map_err(|e| {
                FrameForgeError::Export(format!("video encode flush write failed: {e}"))
            })?;
        }
        part_last_pts = new_pts;
        part_last_dts = new_dts;
        *last_pts = new_pts;
        *last_dts = new_dts;
        wrote = true;
    }

    advance_timeline(
        part,
        out_tb,
        base_pts,
        base_dts,
        part_last_pts,
        part_last_dts,
        wrote,
    );
    Ok(())
}

fn advance_timeline(
    part: Part,
    out_tb: ffmpeg_next::Rational,
    base_pts: &mut i64,
    base_dts: &mut i64,
    part_last_pts: i64,
    part_last_dts: i64,
    wrote: bool,
) {
    let tb_sec = out_tb.0 as f64 / out_tb.1 as f64;
    let ticks = if tb_sec > 0.0 {
        (part.duration() / tb_sec).round() as i64
    } else {
        0
    };
    let expected_pts = *base_pts + ticks.max(0);
    let expected_dts = *base_dts + ticks.max(0);
    *base_pts = if wrote {
        expected_pts.max(part_last_pts + 1)
    } else {
        expected_pts
    };
    // DTS does not advance by wall-clock duration for B-frame GOPs: the
    // last decoded frame's DTS may be far behind the presentation time.
    // Advance only to the next decode slot to avoid gaps at joins.
    *base_dts = if wrote {
        (*base_dts).max(part_last_dts + 1)
    } else {
        expected_dts
    };
}

#[allow(clippy::too_many_arguments)]
fn process_audio(
    ictx: &mut format::context::Input,
    octx: &mut format::context::Output,
    video_start_offset_seconds: f64,
    seg_start: f64,
    seg_end: f64,
    audio_transcoders: &mut [Option<AudioTranscoder>],
    cancel: &CancelToken,
) -> Result<()> {
    let seek_target_seconds = video_start_offset_seconds + seg_start;
    let seek_pts = (seek_target_seconds * 1_000_000.0) as i64;
    ictx.seek(seek_pts, i64::MIN..seek_pts)
        .map_err(|e| FrameForgeError::Export(format!("audio seek failed: {e}")))?;

    for t in audio_transcoders.iter_mut().flatten() {
        t.decoder.flush();
    }

    if audio_transcoders.iter().all(Option::is_none) {
        return Ok(());
    }

    for (stream, pkt) in ictx.packets() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }
        let ist_index = stream.index();
        let Some(t) = audio_transcoders
            .get_mut(ist_index)
            .and_then(Option::as_mut)
        else {
            continue;
        };
        let tb = stream.time_base();
        let tb_sec = tb.0 as f64 / tb.1 as f64;
        let start_time = stream.start_time();
        let stream_start_offset = if start_time == i64::MIN {
            0.0
        } else {
            start_time as f64 * tb_sec
        };
        let pkt_seconds = pkt.pts().unwrap_or(0) as f64 * tb_sec - stream_start_offset;
        if pkt_seconds + EPS >= seg_end {
            continue;
        }

        t.decoder.send_packet(&pkt).ok();
        let mut frame = ffmpeg_next::util::frame::Audio::empty();
        while t.decoder.receive_frame(&mut frame).is_ok() {
            let frame_seconds = frame.pts().unwrap_or(0) as f64 * tb_sec - stream_start_offset;
            if frame_seconds < seg_start - EPS || frame_seconds + EPS >= seg_end {
                continue;
            }

            // Trim audio frames that cross the segment end so the encoded
            // timeline matches the (frame-accurate) video cut instead of
            // dragging in samples from beyond seg_end.
            let audio_rate = t.decoder.rate();
            if audio_rate > 0 {
                let frame_samples = frame.samples();
                let frame_end = frame_seconds + frame_samples as f64 / audio_rate as f64;
                if frame_end > seg_end + EPS {
                    let allowed = ((seg_end - frame_seconds) * audio_rate as f64).floor() as usize;
                    if allowed == 0 {
                        continue;
                    }
                    if allowed < frame_samples {
                        frame.set_samples(allowed);
                    }
                }
            }

            // One continuous sample counter across the whole output.
            if let Some(resampler) = t.resampler.as_mut() {
                let mut out_frame = ffmpeg_next::util::frame::Audio::empty();
                resampler
                    .run(&frame, &mut out_frame)
                    .map_err(|e| FrameForgeError::Export(format!("resample failed: {e}")))?;
                let mut to_send = vec![out_frame];
                // Drain internally buffered converted frames.
                loop {
                    let mut extra = ffmpeg_next::util::frame::Audio::empty();
                    match resampler.flush(&mut extra) {
                        Ok(Some(_)) => {
                            if extra.samples() > 0 {
                                to_send.push(extra);
                            } else {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                for mut of in to_send {
                    if of.samples() == 0 {
                        continue;
                    }
                    enqueue_audio_frame(t, octx, &mut of)?;
                }
            } else {
                enqueue_audio_frame(t, octx, &mut frame)?;
            }
        }
    }

    // Flush each audio decoder at the end of the segment so no frames leak into
    // the next segment.  The encoder is shared across segments, so `send_eof` is
    // only called once at the very end of the export.
    for t in audio_transcoders.iter_mut().flatten() {
        t.decoder.flush();
    }

    Ok(())
}

/// Queue one decoded/resampled audio frame and emit as many complete encoder
/// frames as the FIFO now contains.
fn enqueue_audio_frame(
    t: &mut AudioTranscoder,
    octx: &mut format::context::Output,
    frame: &mut ffmpeg_next::util::frame::Audio,
) -> Result<()> {
    if t.fifo.is_none() {
        frame.set_pts(Some(t.next_pts));
        t.next_pts += frame.samples() as i64;
        t.encoder.send_frame(frame).map_err(|e| {
            FrameForgeError::Export(format!(
                "audio send_frame failed (direct, samples={}): {e}",
                frame.samples()
            ))
        })?;
        return write_audio_packets(t, octx);
    }

    {
        let fifo = t.fifo.as_mut().expect("fifo checked above");
        fifo.write_frame(frame)?;
    }
    drain_audio_fifo(t, octx)
}

fn drain_audio_fifo(t: &mut AudioTranscoder, octx: &mut format::context::Output) -> Result<()> {
    let frame_size = t.frame_size;
    if frame_size == 0 {
        return Ok(());
    }

    loop {
        let available = t.fifo.as_ref().map(AudioFifo::size).unwrap_or(0);
        if available < frame_size {
            break;
        }
        let mut out =
            ffmpeg_next::util::frame::Audio::new(t.fifo_format, frame_size, t.fifo_layout);
        let read = t
            .fifo
            .as_mut()
            .expect("fifo checked by caller")
            .read_into(&mut out, frame_size)?;
        if read != frame_size {
            return Err(FrameForgeError::Export(format!(
                "audio FIFO returned {read} of {frame_size} samples"
            )));
        }
        out.set_samples(frame_size);
        out.set_pts(Some(t.next_pts));
        t.next_pts += frame_size as i64;

        t.encoder.send_frame(&out).map_err(|e| {
            FrameForgeError::Export(format!(
                "audio send_frame failed (fifo, samples={}, format={:?}): {e}",
                out.samples(),
                out.format()
            ))
        })?;
        write_audio_packets(t, octx)?;
    }
    Ok(())
}

/// Emit the final partial FIFO frame, padded with silence.  Encoders with a
/// fixed frame size (AAC) reject larger frames but accept one final padded
/// frame, so the tail of the timeline is not dropped.
fn flush_audio_fifo(t: &mut AudioTranscoder, octx: &mut format::context::Output) -> Result<()> {
    let frame_size = t.frame_size;
    if frame_size == 0 {
        return Ok(());
    }

    loop {
        let available = t.fifo.as_ref().map(AudioFifo::size).unwrap_or(0);
        if available == 0 {
            break;
        }
        let take = available.min(frame_size);
        let mut out =
            ffmpeg_next::util::frame::Audio::new(t.fifo_format, frame_size, t.fifo_layout);
        if take < frame_size {
            for plane in 0..out.planes() {
                out.data_mut(plane).fill(0);
            }
        }
        let read = t
            .fifo
            .as_mut()
            .expect("fifo checked by caller")
            .read_into(&mut out, take)?;
        if read != take {
            return Err(FrameForgeError::Export(format!(
                "audio FIFO returned {read} of {take} samples at flush"
            )));
        }
        out.set_samples(frame_size);
        out.set_pts(Some(t.next_pts));
        t.next_pts += take as i64;
        t.encoder
            .send_frame(&out)
            .map_err(|e| FrameForgeError::Export(format!("audio flush send_frame failed: {e}")))?;
        write_audio_packets(t, octx)?;
    }
    Ok(())
}

fn write_audio_packets(t: &mut AudioTranscoder, octx: &mut format::context::Output) -> Result<()> {
    let mut out_pkt = packet::Packet::empty();
    while t.encoder.receive_packet(&mut out_pkt).is_ok() {
        out_pkt.set_stream(t.ost_index);
        out_pkt.set_position(-1);
        let out_tb = octx.stream(t.ost_index).unwrap().time_base();
        out_pkt.rescale_ts(t.time_base, out_tb);
        out_pkt
            .write_interleaved(octx)
            .map_err(|e| FrameForgeError::Export(format!("audio write failed: {e}")))?;
    }
    Ok(())
}

/// Open a video encoder, degrading a requested hardware encoder to software
/// when the runtime cannot open it.  Returns the encoder, optional VAAPI frames
/// context, and the spec that actually opened.
fn open_encoder_with_fallback(
    spec: &VideoEncoderSpec,
    params: &VideoEncoderParams,
    quality_preset: &str,
    requested: &str,
) -> Result<(
    encoder::Video,
    Option<crate::vaapi::VaapiFrames>,
    VideoEncoderSpec,
)> {
    let mut spec = spec.clone();
    match open_video_encoder(
        codec::Context::new_with_codec(spec.codec),
        params,
        &spec,
        quality_preset,
    ) {
        Ok((enc, hw)) => Ok((enc, hw, spec)),
        Err(e) => {
            if !spec.hardware {
                return Err(e);
            }
            let failed = spec.name.clone();
            let sw = software_h264().ok_or_else(|| {
                FrameForgeError::Export(format!(
                    "hardware encoder '{failed}' failed ({e}) and no software H.264 encoder is available"
                ))
            })?;
            let reason = format!("'{failed}' failed at runtime ({e}); using software H.264");
            spec = VideoEncoderSpec {
                name: sw.name().to_string(),
                codec: sw,
                hardware: false,
                fallback_reason: Some(reason),
            };
            set_last_encoder_report(EncoderReport {
                requested: requested.to_string(),
                encoder: spec.name.clone(),
                hardware: false,
                fallback_reason: spec.fallback_reason.clone(),
            });
            let (enc, hw) = open_video_encoder(
                codec::Context::new_with_codec(spec.codec),
                params,
                &spec,
                quality_preset,
            )?;
            Ok((enc, hw, spec))
        }
    }
}

unsafe fn codec_extradata(ptr: *const ffmpeg_next::ffi::AVCodecContext) -> Option<Vec<u8>> {
    let p = (*ptr).extradata;
    let size = (*ptr).extradata_size;
    if p.is_null() || size <= 0 {
        return None;
    }
    Some(std::slice::from_raw_parts(p, size as usize).to_vec())
}

unsafe fn parameters_extradata(ptr: *const ffmpeg_next::ffi::AVCodecParameters) -> Option<Vec<u8>> {
    let p = (*ptr).extradata;
    let size = (*ptr).extradata_size;
    if p.is_null() || size <= 0 {
        return None;
    }
    Some(std::slice::from_raw_parts(p, size as usize).to_vec())
}

/// Convert an AVCDecoderConfigurationRecord (`avcC`) into the sequence of
/// length-prefixed NAL units (SPS/PPS) used inside `avc3` samples.
/// Split an H.264 extradata blob (AVCDecoderConfigurationRecord or Annex B)
/// into its SPS and PPS NAL units.
fn extract_parameter_sets(extradata: &[u8]) -> Option<(Vec<Vec<u8>>, Vec<Vec<u8>>)> {
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    if extradata.is_empty() {
        return None;
    }

    if extradata.starts_with(&[0, 0, 0, 1]) || extradata.starts_with(&[0, 0, 1]) {
        let mut i = 0;
        while i + 3 <= extradata.len() {
            let nalu_start = if extradata[i..].starts_with(&[0, 0, 0, 1]) {
                i + 4
            } else if extradata[i..].starts_with(&[0, 0, 1]) {
                i + 3
            } else {
                i += 1;
                continue;
            };
            let mut j = nalu_start;
            while j < extradata.len() {
                if j + 3 <= extradata.len() && extradata[j..].starts_with(&[0, 0, 0, 1]) {
                    break;
                }
                if j + 2 <= extradata.len() && extradata[j..].starts_with(&[0, 0, 1]) {
                    break;
                }
                j += 1;
            }
            let nalu = &extradata[nalu_start..j];
            let nal_type = nalu.first().map(|b| b & 0x1f).unwrap_or(0);
            if nal_type == 7 {
                sps.push(nalu.to_vec());
            } else if nal_type == 8 {
                pps.push(nalu.to_vec());
            }
            i = j;
        }
    } else if extradata[0] == 1 {
        let num_sps = (extradata[5] & 0x1f) as usize;
        let mut i = 6;
        for _ in 0..num_sps {
            if i + 2 > extradata.len() {
                return None;
            }
            let len = u16::from_be_bytes([extradata[i], extradata[i + 1]]) as usize;
            i += 2;
            if i + len > extradata.len() {
                return None;
            }
            sps.push(extradata[i..i + len].to_vec());
            i += len;
        }
        if i >= extradata.len() {
            return None;
        }
        let num_pps = extradata[i] as usize;
        i += 1;
        for _ in 0..num_pps {
            if i + 2 > extradata.len() {
                return None;
            }
            let len = u16::from_be_bytes([extradata[i], extradata[i + 1]]) as usize;
            i += 2;
            if i + len > extradata.len() {
                return None;
            }
            pps.push(extradata[i..i + len].to_vec());
            i += len;
        }
    } else {
        return None;
    }

    if sps.is_empty() || pps.is_empty() {
        None
    } else {
        Some((sps, pps))
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn u(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.bit / 8)?;
            let bit = (byte >> (7 - (self.bit % 8))) & 1;
            v = (v << 1) | bit as u32;
            self.bit += 1;
        }
        Some(v)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        if zeros == 0 {
            Some(0)
        } else {
            Some((1 << zeros) - 1 + self.u(zeros)?)
        }
    }

    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        Some(if k % 2 == 1 {
            ((k + 1) / 2) as i32
        } else {
            -((k / 2) as i32)
        })
    }
}

/// Remove H.264 emulation-prevention bytes (`00 00 03` -> `00 00`).
fn nal_rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut i = 0;
    while i < nal.len() {
        if i + 2 < nal.len() && nal[i] == 0 && nal[i + 1] == 0 && nal[i + 2] == 3 {
            out.push(0);
            out.push(0);
            i += 3;
        } else {
            out.push(nal[i]);
            i += 1;
        }
    }
    out
}

/// Return the SPS RBSP bits that precede `vui_parameters_present_flag`.
///
/// Two SPS with identical core bits can be used interchangeably for decoding;
/// the VUI only carries display/timing hints and does not affect slice
/// parsing.  This lets smart rendering use the encoder's `avcC` for copied
/// packets whose SPS differs only in VUI.
fn sps_core_bits(nal: &[u8]) -> Option<Vec<u8>> {
    if nal.len() < 4 {
        return None;
    }
    let rbsp = nal_rbsp(&nal[1..]);
    let mut r = BitReader::new(&rbsp);
    let profile = r.u(8)?;
    r.u(8)?; // constraint flags
    r.u(8)?; // level
    r.ue()?; // sps_id

    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        r.ue()?; // chroma_format_idc
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.u(1)?; // qpprime_y_zero_transform_bypass_flag
        if r.u(1)? == 1 {
            // Scaling matrices are rare; if present, bail out to the safe
            // full re-encode path.
            return None;
        }
    }

    r.ue()?; // log2_max_frame_num_minus4
    let poc_type = r.ue()?;
    if poc_type == 0 {
        r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
    } else if poc_type == 1 {
        r.u(1)?; // delta_pic_order_always_zero_flag
        r.se()?; // offset_for_non_ref_pic
        r.se()?; // offset_for_top_to_bottom_field
        let n = r.ue()?;
        for _ in 0..n {
            r.se()?;
        }
    }

    r.ue()?; // max_num_ref_frames
    r.u(1)?; // gaps_in_frame_num_value_allowed_flag
    r.ue()?; // pic_width_in_mbs_minus1
    r.ue()?; // pic_height_in_map_units_minus1
    let frame_mbs_only = r.u(1)?;
    if frame_mbs_only == 0 {
        r.u(1)?; // mb_adaptive_frame_field_flag
    }
    r.u(1)?; // direct_8x8_inference_flag
    if r.u(1)? == 1 {
        r.ue()?; // frame_crop_left_offset
        r.ue()?; // frame_crop_right_offset
        r.ue()?; // frame_crop_top_offset
        r.ue()?; // frame_crop_bottom_offset
    }

    let core_bits = r.bit;
    let full = core_bits / 8;
    let rem = core_bits % 8;
    let mut out = rbsp[..full].to_vec();
    if rem > 0 {
        let mask = 0xFFu8 << (8 - rem);
        out.push(rbsp[full] & mask);
    }
    Some(out)
}

fn pps_compatible(a: &[u8], b: &[u8]) -> bool {
    fn parse(nal: &[u8]) -> Option<(u32, u32, u32)> {
        if nal.len() < 2 {
            return None;
        }
        let rbsp = nal_rbsp(&nal[1..]);
        let mut r = BitReader::new(&rbsp);
        r.ue()?; // pps_id
        r.ue()?; // sps_id
        let entropy = r.u(1)?;
        r.u(1)?; // bottom_field_pic_order
        let slice_groups = r.ue()?;
        if slice_groups > 0 {
            return None;
        }
        r.ue()?; // num_ref_idx_l0_default_active_minus1
        r.ue()?; // num_ref_idx_l1_default_active_minus1
        let wp = r.u(1)?;
        let wbp = r.u(2)?;
        Some((entropy, wp, wbp))
    }
    let pa = parse(a).map(|(e, _, _)| e);
    let pb = parse(b).map(|(e, _, _)| e);
    match (pa, pb) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Compare SPS fields that affect decoding.  Returns true if the two SPS are
/// compatible for decoding (the decoder can parse the slice headers).
/// Parse SPS into a minimal set of decoded-frame properties.
fn parse_sps_info(nal: &[u8]) -> Option<(u8, u8)> {
    let rbsp = nal_rbsp(&nal[1..]);
    let mut r = BitReader::new(&rbsp);
    let profile = r.u(8)? as u8;
    r.u(8)?; // constraint flags
    let level = r.u(8)? as u8;
    Some((profile, level))
}

fn sps_decode_compatible(a: &[u8], b: &[u8]) -> bool {
    // Parse both SPS into a minimal set of fields.
    fn parse(nal: &[u8]) -> Option<(u8, u8, u32, u32, u8, bool, bool, bool)> {
        let rbsp = nal_rbsp(&nal[1..]);
        let mut r = BitReader::new(&rbsp);
        let profile = r.u(8)? as u8;
        r.u(8)?; // constraint flags
        let level = r.u(8)? as u8;
        r.ue()?; // sps_id

        if matches!(
            profile,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            r.ue()?; // chroma_format_idc
            r.ue()?; // bit_depth_luma_minus8
            r.ue()?; // bit_depth_chroma_minus8
            r.u(1)?; // qpprime_y_zero_transform_bypass_flag
            if r.u(1)? == 1 {
                // scaling matrices present: skip them
                for _ in 0..8 {
                    if r.u(1)? == 1 {
                        let size = 16usize;
                        let mut last = 8i32;
                        for _ in 0..size {
                            let delta = r.se()?;
                            if delta != 0 {
                                last = (last + delta + 256) % 256;
                            }
                        }
                    }
                }
                if profile != 83 && profile != 86 && profile != 118 && profile != 128 {
                    // chroma scaling matrices (only for non-4:4:4)
                }
            }
        }

        let _ = r.ue()?; // log2_max_frame_num_minus4 (skip, GOP size doesn't affect decode)
        let poc_type = r.ue()? as u8;
        if poc_type == 0 {
            r.ue()?; // log2_max_pic_order_cnt_lsb_minus4 (skip)
        } else if poc_type == 1 {
            r.u(1)?;
            r.se()?;
            r.se()?;
            let n = r.ue()?;
            for _ in 0..n {
                r.se()?;
            }
        }
        let _ = r.ue()?; // max_num_ref_frames (skip, decoder tolerates fewer refs)
        r.u(1)?; // gaps_in_frame_num_value_allowed_flag (skip)
        let w = r.ue()?; // pic_width_in_mbs_minus1
        let h = r.ue()?; // pic_height_in_map_units_minus1
        let frame_mbs_only = r.u(1)? == 1;
        if !frame_mbs_only {
            r.u(1)?; // mb_adaptive_frame_field_flag
        }
        let direct_8x8 = r.u(1)? == 1;
        let crop = r.u(1)? == 1;
        let mut crop_l = 0u32;
        let mut crop_r = 0u32;
        let mut crop_t = 0u32;
        let mut crop_b = 0u32;
        if crop {
            crop_l = r.ue()?;
            crop_r = r.ue()?;
            crop_t = r.ue()?;
            crop_b = r.ue()?;
        }
        Some((
            profile,
            level,
            w,
            h,
            poc_type,
            frame_mbs_only,
            direct_8x8,
            crop,
        ))
    }
    let a = match parse(a) {
        Some(v) => v,
        None => return false,
    };
    let b = match parse(b) {
        Some(v) => v,
        None => return false,
    };
    // Compare the fields that affect decoding.
    a.0 == b.0
        && a.1 == b.1
        && a.2 == b.2
        && a.3 == b.3
        && a.4 == b.4
        && a.5 == b.5
        && a.6 == b.6
        && a.7 == b.7
}

/// Extract weighted_pred and weighted_bipred settings from the source's PPS.
/// Returns (weightp, weightb, cabac) for x264.
/// Parse weighted_pred and weighted_bipred_idc from a PPS NAL unit.
/// Returns (weightp, weightb, cabac) for x264.

fn parse_pps_refs(pps: &[u8]) -> Option<usize> {
    let rbsp = nal_rbsp(&pps[1..]);
    let mut r = BitReader::new(&rbsp);
    r.ue(); // pps_id
    r.ue(); // sps_id
    r.u(1); // entropy
    r.u(1); // bottom_field
    let slice_groups = r.ue().unwrap_or(0);
    if slice_groups > 0 {
        return None;
    }
    let ref_l0 = r.ue().unwrap_or(0);
    Some(ref_l0 as usize + 1)
}

fn parse_weighted_pred(pps: &[u8]) -> (u32, u32, bool) {
    let rbsp = nal_rbsp(&pps[1..]);
    let mut r = BitReader::new(&rbsp);
    r.ue(); // pps_id
    r.ue(); // sps_id
    let entropy = r.u(1).unwrap_or(0); // entropy_coding_mode
    r.u(1); // bottom_field_pic_order
    let slice_groups = r.ue().unwrap_or(0);
    if slice_groups > 0 {
        return (1, 0, entropy == 1);
    }
    r.ue(); // num_ref_idx_l0
    r.ue(); // num_ref_idx_l1
    let wp = r.u(1).unwrap_or(0);
    let wbp = r.u(2).unwrap_or(0);
    let weightp = if wp == 1 {
        if wbp == 2 {
            2
        } else {
            1
        }
    } else {
        0
    };
    let weightb = if wbp == 2 { 1 } else { 0 };
    (weightp, weightb, entropy == 1)
}

fn profile_name(id: i32) -> Option<String> {
    match id {
        66 | 578 => Some("baseline".into()),
        77 => Some("main".into()),
        100 => Some("high".into()),
        110 => Some("high10".into()),
        122 => Some("high422".into()),
        244 => Some("high444".into()),
        _ => None,
    }
}

fn pixel_format_name(p: ffmpeg_next::format::Pixel) -> String {
    format!("{:?}", p).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use frameforge_core_types::{Timebase, Timestamp};

    fn idx(seconds: &[f64]) -> KeyframeIndex {
        let tb = Timebase::new(1, 1000);
        KeyframeIndex {
            entries: seconds
                .iter()
                .map(|s| frameforge_media_probe::KeyframeEntry {
                    pts: Timestamp::from_seconds(*s, tb),
                    file_offset: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn split_exact_keyframes_is_all_copy() {
        let idx = idx(&[0.0, 5.0, 10.0, 15.0, 20.0]);
        let parts = split_segment(&idx, 5.0, 15.0);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, PartKind::Copy);
        assert!((parts[0].start - 5.0).abs() < 1e-9);
        assert!((parts[0].end - 15.0).abs() < 1e-9);
    }

    #[test]
    fn split_middle_cut_has_reencode_copy_reencode() {
        let idx = idx(&[0.0, 5.0, 10.0, 15.0, 20.0]);
        let parts = split_segment(&idx, 7.0, 17.0);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].kind, PartKind::Reencode);
        assert!((parts[0].start - 7.0).abs() < 1e-9);
        assert!((parts[0].end - 10.0).abs() < 1e-9);
        assert_eq!(parts[1].kind, PartKind::Copy);
        assert!((parts[1].start - 10.0).abs() < 1e-9);
        assert!((parts[1].end - 15.0).abs() < 1e-9);
        assert_eq!(parts[2].kind, PartKind::Reencode);
        assert!((parts[2].start - 15.0).abs() < 1e-9);
        assert!((parts[2].end - 17.0).abs() < 1e-9);
    }

    #[test]
    fn split_inside_one_gop_is_one_reencode() {
        let idx = idx(&[0.0, 10.0, 20.0]);
        let parts = split_segment(&idx, 3.0, 7.0);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, PartKind::Reencode);
        assert!((parts[0].start - 3.0).abs() < 1e-9);
        assert!((parts[0].end - 7.0).abs() < 1e-9);
    }

    #[test]
    fn split_start_keyframe_end_inside() {
        let idx = idx(&[0.0, 5.0, 10.0, 15.0]);
        let parts = split_segment(&idx, 5.0, 12.0);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].kind, PartKind::Copy);
        assert!((parts[0].end - 10.0).abs() < 1e-9);
        assert_eq!(parts[1].kind, PartKind::Reencode);
        assert!((parts[1].start - 10.0).abs() < 1e-9);
        assert!((parts[1].end - 12.0).abs() < 1e-9);
    }
}
