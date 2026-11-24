//! export-engine: stream-copy remux + re-encode pipeline orchestration.
//!
//! Atomic output (temp file + rename), disk-space precheck, progress, cancellation.
//!
//! Stream-copy uses ffmpeg-next directly (packet-by-packet remux), never loading
//! whole files into RAM. Re-encode uses decode → normalize → encode pipeline.

use ffmpeg_next::{codec, encoder, media, packet, Codec, Dictionary, Rational as FfmpegRational};
use frameforge_core_types::{Clip, FrameForgeError, PathBuf, Result, SourceId};
use frameforge_edit_engine::{ExportPlan, PlannedSegment};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

pub(crate) mod vaapi;

mod smart;
pub use smart::SmartRenderExecutor;

mod ui;
pub use ui::{estimate_export_seconds, export_reason, lossless_badge_visible, ExportPath};

pub type ProgressCallback = dyn Fn(f32) + Send + Sync;

/// Description of the video encoder that actually ran.
///
/// Hardware preferences can silently degrade to software when the runtime lacks
/// the device or the ffmpeg build lacks the encoder, so the resolved choice is
/// reported instead of being assumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderReport {
    /// Encoder preference requested by the caller (e.g. `nvenc`, `x264`).
    pub requested: String,
    /// FFmpeg encoder name actually used (e.g. `libx264`, `h264_nvenc`).
    pub encoder: String,
    /// Whether the encoder is a hardware (offloaded) implementation.
    pub hardware: bool,
    /// Human-readable reason when a requested encoder was unavailable and a
    /// software fallback was used instead.
    pub fallback_reason: Option<String>,
}

static LAST_ENCODER_REPORT: OnceLock<Mutex<Option<EncoderReport>>> = OnceLock::new();

/// Encoder resolved by the most recent re-encode export, if any.
pub fn last_encoder_report() -> Option<EncoderReport> {
    LAST_ENCODER_REPORT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|g| g.clone())
}

/// Forget the last encoder report (call before starting a new export).
pub fn clear_last_encoder_report() {
    if let Some(m) = LAST_ENCODER_REPORT.get() {
        if let Ok(mut g) = m.lock() {
            *g = None;
        }
    }
}

pub fn set_last_encoder_report(report: EncoderReport) {
    let m = LAST_ENCODER_REPORT.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = m.lock() {
        *g = Some(report);
    }
}

#[derive(Debug, Clone)]
pub struct CancelToken {
    cancelled: std::sync::Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            cancelled: std::sync::Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

pub trait ExportExecutor {
    fn run(
        &mut self,
        plan: &ExportPlan,
        progress: &ProgressCallback,
        cancel: &CancelToken,
    ) -> Result<()>;
}

/// Stream-copy executor: packet-by-packet remux, timestamp rebasing, never loads
/// whole files into RAM.
pub struct StreamCopyExecutor {
    clips: Vec<Clip>,
    destination: PathBuf,
}

impl StreamCopyExecutor {
    pub fn new(clips: Vec<Clip>, destination: PathBuf) -> Self {
        Self { clips, destination }
    }

    fn build_clip_map(&self) -> HashMap<SourceId, PathBuf> {
        self.clips.iter().map(|c| (c.id, c.path.clone())).collect()
    }
}

impl ExportExecutor for StreamCopyExecutor {
    fn run(
        &mut self,
        plan: &ExportPlan,
        progress: &ProgressCallback,
        cancel: &CancelToken,
    ) -> Result<()> {
        ffmpeg_next::init()
            .map_err(|e| FrameForgeError::Probe(format!("ffmpeg init failed: {e}")))?;

        if plan.segments.is_empty() {
            return Err(FrameForgeError::Export("no segments to export".into()));
        }

        let output_format = output_format_from_extension(&self.destination);
        let clip_map = self.build_clip_map();
        let estimate = estimate_output_bytes(plan, &clip_map)?;
        ensure_disk_space(&self.destination, estimate)?;

        atomic_write(&self.destination, |temp_path, fmt| {
            remux_multiple_files(
                temp_path,
                plan,
                cancel,
                progress,
                fmt.or(output_format),
                &clip_map,
            )
        })?;

        Ok(())
    }
}

impl StreamCopyExecutor {}

/// Remux segments from multiple input files into a single output file.
fn remux_multiple_files(
    output_path: &Path,
    plan: &ExportPlan,
    cancel: &CancelToken,
    progress: &ProgressCallback,
    output_format: Option<&'static str>,
    clip_map: &HashMap<SourceId, PathBuf>,
) -> Result<()> {
    // Group segments by source_id to minimize file opens/closes
    let mut segments_by_source: HashMap<SourceId, Vec<&PlannedSegment>> = HashMap::new();
    for segment in &plan.segments {
        segments_by_source
            .entry(segment.source_id)
            .or_default()
            .push(segment);
    }

    // Create output context first
    let mut octx = match output_format.or(Some("mp4")) {
        Some(fmt) => ffmpeg_next::format::output_as(output_path, fmt)
            .map_err(|e| FrameForgeError::Export(format!("format::output_as failed: {e}")))?,
        None => ffmpeg_next::format::output(output_path)
            .map_err(|e| FrameForgeError::Export(format!("format::output failed: {e}")))?,
    };

    // We'll build stream mapping from the first input file that has video/audio streams
    // For simplicity, we assume all clips have compatible streams (validated by edit-engine)
    let mut video_stream_idx: Option<usize> = None;
    let mut ost_index = 0;

    // Use the first clip to set up output streams
    let first_source = plan
        .segments
        .first()
        .map(|s| s.source_id)
        .ok_or_else(|| FrameForgeError::Export("no segments in plan".into()))?;
    let first_clip_path = clip_map
        .get(&first_source)
        .ok_or_else(|| FrameForgeError::Export("no clips in map".into()))?;

    let first_ictx = ffmpeg_next::format::input(first_clip_path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

    let input_streams: Vec<ffmpeg_next::Stream> = first_ictx.streams().collect();
    let mut stream_mapping = vec![-1; input_streams.len()];

    for (ist_index, ist) in input_streams.iter().enumerate() {
        let medium = ist.parameters().medium();
        if medium != media::Type::Video
            && medium != media::Type::Audio
            && medium != media::Type::Subtitle
        {
            continue;
        }
        if medium == media::Type::Video {
            video_stream_idx = Some(ist_index);
        }
        stream_mapping[ist_index] = ost_index as i32;
        ost_index += 1;

        let mut ost = octx
            .add_stream(ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::None))
            .map_err(|e| FrameForgeError::Export(format!("add_stream failed: {e}")))?;
        ost.set_parameters(ist.parameters());
        // SAFETY: ost.parameters() returns a valid Parameters object from the ffmpeg-next
        // output stream. The mutable pointer is safe to dereference because:
        // 1. The stream was just created by add_stream() and is guaranteed to be valid
        // 2. We only modify the codec_tag field which is a simple integer (u32)
        // 3. The Parameters object outlives this operation as it's owned by the stream
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
        }
    }

    if ost_index == 0 {
        return Err(FrameForgeError::Export(
            "no audio/video/subtitle streams to export".into(),
        ));
    }

    octx.set_metadata(first_ictx.metadata().to_owned());
    let mut hdr_opts = ffmpeg_next::Dictionary::new();
    hdr_opts.set("avoid_negative_ts", "make_non_negative");
    octx.write_header_with(hdr_opts)
        .map_err(|e| FrameForgeError::Export(format!("write_header failed: {e}")))?;

    let mut base_offsets: Vec<i64> = vec![0; ost_index];
    let mut base_dts_offsets: Vec<i64> = vec![0; ost_index];
    let mut last_dts: Vec<Option<i64>> = vec![None; ost_index];
    let mut stream_seg_start_pts: Vec<Option<i64>> = vec![None; input_streams.len()];
    let mut stream_seg_start_dts: Vec<Option<i64>> = vec![None; input_streams.len()];

    let total_segments = plan.segments.len() as f32;

    // Global segment counter for progress reporting.

    // Process segments in timeline order. Each segment opens its own input file
    // so multi-source timelines keep the user's intended order.
    for (seg_idx, segment) in plan.segments.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }

        let file_path = clip_map.get(&segment.source_id).ok_or_else(|| {
            FrameForgeError::Export(format!(
                "clip not found for source_id: {}",
                segment.source_id.0
            ))
        })?;

        let mut ictx = ffmpeg_next::format::input(file_path).map_err(|e| {
            FrameForgeError::Probe(format!(
                "format::input failed for {}: {e}",
                file_path.display()
            ))
        })?;

        // Reset per-stream segment start.
        stream_seg_start_pts.iter_mut().for_each(|s| *s = None);
        stream_seg_start_dts.iter_mut().for_each(|s| *s = None);

        let mut ctx = RemuxCtx {
            ictx: &mut ictx,
            octx: &mut octx,
            stream_mapping: &stream_mapping,
            base_offsets: &mut base_offsets,
            base_dts_offsets: &mut base_dts_offsets,
            last_dts: &mut last_dts,
            stream_seg_start_pts: &mut stream_seg_start_pts,
            stream_seg_start_dts: &mut stream_seg_start_dts,
            video_stream_idx,
        };

        remux_segment(&mut ctx, segment, cancel)?;

        // Report progress using a global segment counter so the bar never
        // regresses.
        progress((seg_idx + 1) as f32 / total_segments);
    }

    octx.write_trailer()
        .map_err(|e| FrameForgeError::Export(format!("write_trailer failed: {e}")))?;

    Ok(())
}

/// Re-encode executor: decode -> normalize -> encode (VAAPI/NVENC or sw fallback).
pub struct ReencodeExecutor {
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
}

impl ReencodeExecutor {
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

    fn build_clip_map(&self) -> HashMap<SourceId, PathBuf> {
        self.clips.iter().map(|c| (c.id, c.path.clone())).collect()
    }
}

impl ExportExecutor for ReencodeExecutor {
    fn run(
        &mut self,
        plan: &ExportPlan,
        progress: &ProgressCallback,
        cancel: &CancelToken,
    ) -> Result<()> {
        ffmpeg_next::init()
            .map_err(|e| FrameForgeError::Probe(format!("ffmpeg init failed: {e}")))?;

        if plan.segments.is_empty() {
            return Err(FrameForgeError::Export("no segments to export".into()));
        }

        let output_format = output_format_from_extension(&self.destination);
        if matches!(output_format, Some("mp4") | Some("mov")) {
            // Use the smart executor's re-encode path: it has the correct
            // 0-based PTS/DTS rebasing and continuous audio timeline.
            let mut exec = crate::SmartRenderExecutor::new(
                self.clips.clone(),
                self.destination.clone(),
                self.encoder_pref.clone(),
                self.quality_preset.clone(),
            )
            .with_normalization(
                self.target_width,
                self.target_height,
                self.target_fps_num,
                self.target_fps_den,
                self.target_pix_fmt.clone(),
                self.audio_sample_rate,
                self.audio_channels,
            )
            .with_force_reencode(true);
            return exec.run(plan, progress, cancel);
        }

        let clip_map = self.build_clip_map();
        let estimate = estimate_output_bytes(plan, &clip_map)?;
        ensure_disk_space(&self.destination, estimate)?;

        atomic_write(&self.destination, |temp_path, fmt| {
            reencode_multiple_files(
                temp_path,
                plan,
                cancel,
                progress,
                fmt.or(output_format),
                &clip_map,
                &self.encoder_pref,
                self.target_width,
                self.target_height,
                self.target_fps_num,
                self.target_fps_den,
                self.target_pix_fmt.as_deref(),
                self.audio_sample_rate,
                self.audio_channels,
                &self.quality_preset,
            )
        })?;

        Ok(())
    }
}

/// Core re-encode logic: decode → normalize → encode → mux.
/// Handles multiple input clips by opening each clip's file for its segments.
#[allow(clippy::too_many_arguments)]
fn reencode_multiple_files(
    output_path: &Path,
    plan: &ExportPlan,
    cancel: &CancelToken,
    progress: &ProgressCallback,
    output_format: Option<&'static str>,
    clip_map: &HashMap<SourceId, PathBuf>,
    encoder_pref: &str,
    target_width: Option<u32>,
    target_height: Option<u32>,
    target_fps_num: Option<u32>,
    target_fps_den: Option<u32>,
    target_pix_fmt: Option<&str>,
    audio_sample_rate: Option<u32>,
    audio_channels: Option<u32>,
    quality_preset: &str,
) -> Result<()> {
    // Use the first clip to set up output streams and encoders
    let first_source = plan
        .segments
        .first()
        .map(|s| s.source_id)
        .ok_or_else(|| FrameForgeError::Export("no segments in plan".into()))?;
    let first_clip_path = clip_map
        .get(&first_source)
        .ok_or_else(|| FrameForgeError::Export("no clips in map".into()))?;

    let first_ictx = ffmpeg_next::format::input(first_clip_path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

    let mut octx = match output_format.or(Some("mp4")) {
        Some(fmt) => ffmpeg_next::format::output_as(output_path, fmt)
            .map_err(|e| FrameForgeError::Export(format!("format::output_as failed: {e}")))?,
        None => ffmpeg_next::format::output(output_path)
            .map_err(|e| FrameForgeError::Export(format!("format::output failed: {e}")))?,
    };

    let input_streams: Vec<ffmpeg_next::Stream> = first_ictx.streams().collect();
    let mut stream_mapping: Vec<i32> = vec![-1; input_streams.len()];
    let mut ist_time_bases: Vec<FfmpegRational> = vec![FfmpegRational(1, 1); input_streams.len()];

    // Resolve normalization targets to the first clip's stream parameters when
    // the caller did not provide explicit ones. This is what makes a mixed
    // resolution / mixed frame-rate / mixed pixel-format export actually
    // normalize every input to one common output format instead of silently
    // encoding each source at its own parameters (which produces a corrupt
    // or variable-geometry output).
    let first_video_stream = input_streams
        .iter()
        .find(|s| s.parameters().medium() == media::Type::Video);
    let first_audio_stream = input_streams
        .iter()
        .find(|s| s.parameters().medium() == media::Type::Audio);

    let target_width = target_width.or_else(|| {
        first_video_stream.and_then(|s| {
            let w = unsafe { (*s.parameters().as_ptr()).width };
            if w > 0 {
                Some(w as u32)
            } else {
                None
            }
        })
    });
    let target_height = target_height.or_else(|| {
        first_video_stream.and_then(|s| {
            let h = unsafe { (*s.parameters().as_ptr()).height };
            if h > 0 {
                Some(h as u32)
            } else {
                None
            }
        })
    });
    let target_fps_num = target_fps_num.or_else(|| {
        first_video_stream
            .map(|s| s.rate().numerator() as u32)
            .filter(|&n| n != 0)
    });
    let target_fps_den = target_fps_den.or_else(|| {
        first_video_stream
            .map(|s| s.rate().denominator() as u32)
            .filter(|&d| d != 0)
    });
    let mut resolved_pix_fmt: Option<String> = target_pix_fmt.map(str::to_string);
    let audio_sample_rate = audio_sample_rate.or_else(|| {
        first_audio_stream.and_then(|s| {
            let r = unsafe { (*s.parameters().as_ptr()).sample_rate };
            if r > 0 {
                Some(r as u32)
            } else {
                None
            }
        })
    });
    let audio_channels = audio_channels.or_else(|| {
        first_audio_stream.and_then(|s| {
            let c = unsafe { (*s.parameters().as_ptr()).channels };
            if c > 0 {
                Some(c as u32)
            } else {
                None
            }
        })
    });

    struct VideoTranscoder {
        decoder: codec::decoder::Video,
        encoder: encoder::Video,
        ost_index: usize,
        /// Present when the encoder consumes VAAPI hardware frames.
        hw: Option<vaapi::VaapiFrames>,
    }

    struct AudioTranscoder {
        decoder: codec::decoder::Audio,
        encoder: encoder::Audio,
        ost_index: usize,
        /// Encoder timebase (1 / sample rate).
        time_base: ffmpeg_next::Rational,
        /// Continuous sample counter across the whole output.
        next_pts: i64,
    }

    let mut video_transcoders: Vec<Option<VideoTranscoder>> =
        (0..input_streams.len()).map(|_| None).collect();
    let mut audio_transcoders: Vec<Option<AudioTranscoder>> =
        (0..input_streams.len()).map(|_| None).collect();

    // Resolve the requested video encoder once for the whole file. A hardware
    // preference that is not usable in this environment degrades to software
    // with the reason recorded in the encoder report.
    let mut active_video_spec = resolve_video_encoder(encoder_pref)?;
    set_last_encoder_report(EncoderReport {
        requested: encoder_pref.to_string(),
        encoder: active_video_spec.name.clone(),
        hardware: active_video_spec.hardware,
        fallback_reason: active_video_spec.fallback_reason.clone(),
    });

    let mut ost_index = 0;
    for (ist_index, ist) in input_streams.iter().enumerate() {
        let medium = ist.parameters().medium();
        if medium != media::Type::Video && medium != media::Type::Audio {
            continue;
        }

        ist_time_bases[ist_index] = ist.time_base();
        stream_mapping[ist_index] = ost_index as i32;

        let codec = match medium {
            media::Type::Video => active_video_spec.codec,
            media::Type::Audio => select_audio_encoder(encoder_pref)?,
            _ => continue,
        };

        // Create decoder. Use `.video()`/`.audio()` (not `.open()`) so ffmpeg
        // resolves and opens the codec from the stream parameters.
        let dec_ctx = codec::Context::from_parameters(ist.parameters())
            .map_err(|e| FrameForgeError::Export(format!("decoder context failed: {e}")))?;

        if medium == media::Type::Video {
            let mut dec_video = dec_ctx
                .decoder()
                .video()
                .map_err(|e| FrameForgeError::Export(format!("decoder video failed: {e}")))?;
            dec_video.set_parameters(ist.parameters()).ok();

            let width = target_width.unwrap_or(dec_video.width());
            let height = target_height.unwrap_or(dec_video.height());

            // Pixel format of the software frames handed to the encoder after
            // normalization.
            if resolved_pix_fmt.is_none() {
                resolved_pix_fmt = Some(format!("{:?}", dec_video.format()).to_lowercase());
            }
            let sw_format = resolved_pix_fmt
                .as_deref()
                .and_then(parse_pixel_format)
                .unwrap_or(dec_video.format());

            let frame_rate = match (target_fps_num, target_fps_den) {
                (Some(num), Some(den)) => {
                    ffmpeg_next::Rational(num.try_into().unwrap_or(0), den.try_into().unwrap_or(1))
                }
                _ => ist.rate(),
            };

            // VAAPI needs its frames context attached before the codec is
            // opened (see `vaapi` module). If the encoder cannot be opened at
            // all — e.g. the driver has no encode entrypoint — fall back to
            // software instead of failing the whole export.
            let params = VideoEncoderParams {
                width,
                height,
                sw_format,
                frame_rate,
                time_base: ist.time_base(),
                bframes: None,
                profile: None,
                level: None,
                x264_params: None,
                refs: None,
                gop_size: None,
                global_header: true,
            };

            let (opened_encoder, hw) = match open_video_encoder(
                codec::Context::new_with_codec(active_video_spec.codec),
                &params,
                &active_video_spec,
                quality_preset,
            ) {
                Ok(v) => v,
                Err(e) => {
                    if !active_video_spec.hardware {
                        return Err(e);
                    }
                    let failed = active_video_spec.name.clone();
                    let sw = software_h264().ok_or_else(|| {
                        FrameForgeError::Export(format!(
                            "hardware encoder '{failed}' failed ({e}) and no software H.264 \
                             encoder is available"
                        ))
                    })?;
                    let reason =
                        format!("'{failed}' failed at runtime ({e}); using software H.264");
                    active_video_spec = VideoEncoderSpec {
                        name: sw.name().to_string(),
                        codec: sw,
                        hardware: false,
                        fallback_reason: Some(reason.clone()),
                    };
                    set_last_encoder_report(EncoderReport {
                        requested: encoder_pref.to_string(),
                        encoder: active_video_spec.name.clone(),
                        hardware: false,
                        fallback_reason: Some(reason),
                    });
                    open_video_encoder(
                        codec::Context::new_with_codec(active_video_spec.codec),
                        &params,
                        &active_video_spec,
                        quality_preset,
                    )?
                }
            };

            // Use the spec that actually opened the encoder: on a hardware
            // fallback `active_video_spec` has been replaced with software.
            let mut ost = octx
                .add_stream(active_video_spec.codec)
                .map_err(|e| FrameForgeError::Export(format!("add_stream failed: {e}")))?;
            ost.set_parameters(&opened_encoder);
            ost.set_time_base(ist.time_base());
            // SAFETY: ...
            unsafe {
                (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            }

            video_transcoders[ist_index] = Some(VideoTranscoder {
                decoder: dec_video,
                encoder: opened_encoder,
                ost_index,
                hw,
            });
        } else if medium == media::Type::Audio {
            let mut dec_audio = dec_ctx
                .decoder()
                .audio()
                .map_err(|e| FrameForgeError::Export(format!("decoder audio failed: {e}")))?;
            dec_audio.set_parameters(ist.parameters()).ok();
            let enc_ctx = codec::Context::new_with_codec(codec);
            let mut enc_audio = enc_ctx
                .encoder()
                .audio()
                .map_err(|e| FrameForgeError::Export(format!("encoder audio failed: {e}")))?;
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
            enc_audio.set_rate(rate as i32);

            let channels = audio_channels.unwrap_or_else(|| {
                if src_channels > 0 {
                    src_channels as u32
                } else if dec_audio.channels() > 0 {
                    dec_audio.channels() as u32
                } else {
                    2
                }
            });
            enc_audio.set_channels(channels as i32);

            let layout = if src_channel_layout != 0
                && src_channels > 0
                && src_channels == channels as i32
            {
                ffmpeg_next::channel_layout::ChannelLayout::from_bits_truncate(src_channel_layout)
            } else if channels == 1 {
                ffmpeg_next::channel_layout::ChannelLayout::MONO
            } else {
                ffmpeg_next::channel_layout::ChannelLayout::STEREO
            };
            enc_audio.set_channel_layout(layout);

            enc_audio.set_time_base(ffmpeg_next::Rational(1, rate as i32));
            enc_audio.set_format(dec_audio.format());

            let opened_encoder = enc_audio
                .open_with(Dictionary::new())
                .map_err(|e| FrameForgeError::Export(format!("encoder open failed: {e}")))?;

            let mut ost = octx
                .add_stream(codec)
                .map_err(|e| FrameForgeError::Export(format!("add_stream failed: {e}")))?;
            ost.set_parameters(&opened_encoder);
            ost.set_time_base(ffmpeg_next::Rational(1, rate as i32));
            // SAFETY: ...
            unsafe {
                (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            }

            audio_transcoders[ist_index] = Some(AudioTranscoder {
                decoder: dec_audio,
                encoder: opened_encoder,
                ost_index,
                time_base: ffmpeg_next::Rational(1, rate as i32),
                next_pts: 0,
            });
        }

        ost_index += 1;
    }

    if ost_index == 0 {
        return Err(FrameForgeError::Export(
            "no audio/video streams to export".into(),
        ));
    }

    octx.set_metadata(first_ictx.metadata().to_owned());
    octx.write_header()
        .map_err(|e| FrameForgeError::Export(format!("write_header failed: {e}")))?;

    let total_segments = plan.segments.len() as f32;
    let mut base_offsets: Vec<i64> = vec![0; ost_index];
    let mut base_dts_offsets: Vec<i64> = vec![0; ost_index];

    // Process each segment in timeline order
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

        // Open input file for this segment's clip
        let mut ictx = ffmpeg_next::format::input(clip_path)
            .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

        // Seek to segment start using cut_points (keyframe-snapped) when available.
        // Timestamps are media-relative; normalize against the container start_time
        // (e.g. MPEG-TS) so cuts do not shift by hours.
        let seg_start_seconds = segment
            .cut_points
            .first()
            .map(|t| t.to_seconds())
            .unwrap_or_else(|| segment.range.start.to_seconds());

        let start_offset_seconds = ictx
            .streams()
            .find(|s| s.parameters().medium() == media::Type::Video)
            .map(|s| {
                let st = s.start_time();
                if st == i64::MIN {
                    0.0
                } else {
                    st as f64 * (s.time_base().0 as f64 / s.time_base().1 as f64)
                }
            })
            .unwrap_or(0.0);

        let seek_target_seconds = start_offset_seconds + seg_start_seconds;
        let start_pts_av = (seek_target_seconds * 1_000_000.0) as i64;

        // Find video stream index in this file for seeking
        let has_video = ictx
            .streams()
            .any(|s| s.parameters().medium() == media::Type::Video);

        if has_video {
            ictx.seek(start_pts_av, i64::MIN..start_pts_av)
                .map_err(|e| FrameForgeError::Export(format!("seek failed: {e}")))?;
        }

        // Flush decoders after seek so no stale frames leak between segments.
        for t in video_transcoders.iter_mut().flatten() {
            t.decoder.flush();
        }
        for t in audio_transcoders.iter_mut().flatten() {
            t.decoder.flush();
        }

        let seg_end_seconds = segment
            .cut_points
            .get(1)
            .map(|t| t.to_seconds())
            .unwrap_or_else(|| segment.range.end.to_seconds());

        // Output timeline position at the start of this segment.  The source
        // timestamp is never added here: the encoder is fed a 0-based PTS for
        // the segment and the output is a concatenation of the kept ranges.
        let seg_base_pts: Vec<i64> = base_offsets.clone();
        let seg_base_dts: Vec<i64> = base_dts_offsets.clone();
        let mut local_frame_pts: Vec<i64> = vec![0; input_streams.len()];

        // Process packets for this segment
        for (stream, pkt) in ictx.packets() {
            if cancel.is_cancelled() {
                return Err(FrameForgeError::Export("export cancelled".into()));
            }

            let ist_index = stream.index();
            let ost_index = stream_mapping[ist_index];
            if ost_index < 0 {
                continue;
            }

            let pkt_pts = pkt.pts().unwrap_or(0);
            let pkt_seconds = pkt_pts as f64
                * (ist_time_bases[ist_index].0 as f64 / ist_time_bases[ist_index].1 as f64)
                - start_offset_seconds;

            // Keep feeding the decoder for packets before seg_start so reference
            // frames (keyframes) are available; those frames are discarded below
            // instead of being encoded. Skipping them here would leave the
            // decoder without its reference and produce blocky output.
            // End boundary is exclusive: stop once a packet reaches (or is
            // within floating-point epsilon of) the segment end.
            if pkt_seconds + 1e-9 >= seg_end_seconds {
                break;
            }

            let medium = stream.parameters().medium();

            if medium == media::Type::Video {
                if let Some(t) = video_transcoders[ist_index].as_mut() {
                    t.decoder.send_packet(&pkt).ok();
                    let frame_time_base = stream.time_base();
                    let mut frame = ffmpeg_next::util::frame::Video::empty();
                    while t.decoder.receive_frame(&mut frame).is_ok() {
                        // Decode-and-discard: keep reference frames before the
                        // cut (needed for correct decoding) but only encode
                        // frames that fall inside the requested range.
                        let frame_seconds = frame.pts().unwrap_or(0) as f64
                            * (frame_time_base.0 as f64 / frame_time_base.1 as f64)
                            - start_offset_seconds;
                        if frame_seconds < seg_start_seconds - 1e-9
                            || frame_seconds + 1e-9 >= seg_end_seconds
                        {
                            continue;
                        }
                        let mut norm_frame = normalize_video_frame(
                            &frame,
                            target_width,
                            target_height,
                            resolved_pix_fmt.as_deref(),
                        )?;
                        // 0-based PTS for this segment.  Without this the
                        // encoder emits source timestamps and every segment
                        // join accumulates a gap.
                        let rate = stream.rate();
                        let step = if rate.numerator() > 0
                            && rate.denominator() > 0
                            && stream.time_base().numerator() > 0
                        {
                            (stream.time_base().denominator() as i64 * rate.denominator() as i64)
                                / (stream.time_base().numerator() as i64 * rate.numerator() as i64)
                        } else {
                            1
                        };
                        norm_frame.set_pts(Some(local_frame_pts[ist_index]));
                        local_frame_pts[ist_index] += step.max(1);
                        if let Some(hw) = t.hw.as_ref() {
                            // SAFETY: `hw` was attached to this transcoder's
                            // encoder, which was opened with the VAAPI pixel
                            // format, so the upload target is valid.
                            unsafe { hw.upload(&mut t.encoder, &norm_frame) }
                                .map_err(FrameForgeError::Export)?;
                        } else {
                            t.encoder.send_frame(&norm_frame).ok();
                        }
                        let mut out_pkt = packet::Packet::empty();
                        while t.encoder.receive_packet(&mut out_pkt).is_ok() {
                            out_pkt.set_stream(t.ost_index);
                            out_pkt.set_position(-1);
                            // Rescale the encoder timestamp into the output
                            // timebase first, then add the segment base (which
                            // is already in the output timebase).
                            let out_tb = octx.stream(t.ost_index).unwrap().time_base();
                            out_pkt.rescale_ts(stream.time_base(), out_tb);
                            let mut new_pts =
                                out_pkt.pts().unwrap_or(0) + seg_base_pts[t.ost_index];
                            let mut new_dts =
                                out_pkt.dts().unwrap_or(0) + seg_base_dts[t.ost_index];
                            if new_pts < seg_base_pts[t.ost_index] {
                                new_pts = seg_base_pts[t.ost_index];
                            }
                            if new_dts < seg_base_dts[t.ost_index] {
                                new_dts = seg_base_dts[t.ost_index];
                            }
                            if new_pts <= base_offsets[t.ost_index] {
                                new_pts = base_offsets[t.ost_index] + 1;
                            }
                            if new_dts <= base_dts_offsets[t.ost_index] {
                                new_dts = base_dts_offsets[t.ost_index] + 1;
                            }
                            out_pkt.set_pts(Some(new_pts));
                            out_pkt.set_dts(Some(new_dts));
                            base_offsets[t.ost_index] = new_pts;
                            base_dts_offsets[t.ost_index] = new_dts;
                            out_pkt.write_interleaved(&mut octx).map_err(|e| {
                                FrameForgeError::Export(format!("video write failed: {e}"))
                            })?;
                        }
                    }
                }
            } else if medium == media::Type::Audio {
                if let Some(t) = audio_transcoders[ist_index].as_mut() {
                    t.decoder.send_packet(&pkt).ok();
                    let frame_time_base = stream.time_base();
                    let mut frame = ffmpeg_next::util::frame::Audio::empty();
                    while t.decoder.receive_frame(&mut frame).is_ok() {
                        // Decode-and-discard: only encode audio frames inside
                        // the requested range; reference frames before the cut
                        // are still fed to the decoder above.
                        let frame_seconds = frame.pts().unwrap_or(0) as f64
                            * (frame_time_base.0 as f64 / frame_time_base.1 as f64)
                            - start_offset_seconds;
                        if frame_seconds < seg_start_seconds - 1e-9
                            || frame_seconds + 1e-9 >= seg_end_seconds
                        {
                            continue;
                        }
                        // One continuous sample counter for the whole output.
                        frame.set_pts(Some(t.next_pts));
                        t.next_pts += frame.samples() as i64;
                        t.encoder.send_frame(&frame).ok();
                        let mut out_pkt = packet::Packet::empty();
                        while t.encoder.receive_packet(&mut out_pkt).is_ok() {
                            out_pkt.set_stream(t.ost_index);
                            out_pkt.set_position(-1);
                            let out_tb = octx.stream(t.ost_index).unwrap().time_base();
                            out_pkt.rescale_ts(t.time_base, out_tb);
                            // `next_pts` is a single continuous sample counter
                            // across the whole output, so no segment base is
                            // added here.
                            let mut new_pts = out_pkt.pts().unwrap_or(0);
                            let mut new_dts = out_pkt.dts().unwrap_or(0);
                            if new_pts <= base_offsets[t.ost_index] {
                                new_pts = base_offsets[t.ost_index] + 1;
                            }
                            if new_dts <= base_dts_offsets[t.ost_index] {
                                new_dts = base_dts_offsets[t.ost_index] + 1;
                            }
                            out_pkt.set_pts(Some(new_pts));
                            out_pkt.set_dts(Some(new_dts));
                            base_offsets[t.ost_index] = new_pts;
                            base_dts_offsets[t.ost_index] = new_dts;
                            out_pkt.write_interleaved(&mut octx).map_err(|e| {
                                FrameForgeError::Export(format!("audio write failed: {e}"))
                            })?;
                        }
                    }
                }
            }
        }

        // Input context is dropped here, closing the file

        // Report progress after each segment completes.
        progress((seg_idx + 1) as f32 / total_segments);
    }

    // Flush encoders.
    for t in video_transcoders.iter_mut().flatten() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }
        t.encoder.send_eof().ok();
        let mut out_pkt = packet::Packet::empty();
        while t.encoder.receive_packet(&mut out_pkt).is_ok() {
            out_pkt.set_stream(t.ost_index);
            out_pkt.set_position(-1);
            // Flush frames: continue the output timeline monotonically.
            let new_pts = base_offsets[t.ost_index] + 1;
            let new_dts = base_dts_offsets[t.ost_index] + 1;
            out_pkt.set_pts(Some(new_pts));
            out_pkt.set_dts(Some(new_dts));
            out_pkt.rescale_ts(
                octx.stream(t.ost_index).unwrap().time_base(),
                octx.stream(t.ost_index).unwrap().time_base(),
            );
            base_offsets[t.ost_index] = new_pts;
            base_dts_offsets[t.ost_index] = new_dts;
            out_pkt
                .write_interleaved(&mut octx)
                .map_err(|e| FrameForgeError::Export(format!("video write failed: {e}")))?;
        }
    }
    for t in audio_transcoders.iter_mut().flatten() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }
        t.encoder.send_eof().ok();
        let mut out_pkt = packet::Packet::empty();
        while t.encoder.receive_packet(&mut out_pkt).is_ok() {
            out_pkt.set_stream(t.ost_index);
            out_pkt.set_position(-1);
            // Flush frames: continue the output timeline monotonically.
            let new_pts = base_offsets[t.ost_index] + 1;
            let new_dts = base_dts_offsets[t.ost_index] + 1;
            out_pkt.set_pts(Some(new_pts));
            out_pkt.set_dts(Some(new_dts));
            out_pkt.rescale_ts(
                octx.stream(t.ost_index).unwrap().time_base(),
                octx.stream(t.ost_index).unwrap().time_base(),
            );
            base_offsets[t.ost_index] = new_pts;
            base_dts_offsets[t.ost_index] = new_dts;
            out_pkt
                .write_interleaved(&mut octx)
                .map_err(|e| FrameForgeError::Export(format!("audio write failed: {e}")))?;
        }
    }

    octx.write_trailer()
        .map_err(|e| FrameForgeError::Export(format!("write_trailer failed: {e}")))?;

    progress(1.0);
    Ok(())
}

/// Select video encoder by preference name.
/// Resolved video encoder for a preference string.
#[derive(Clone)]
pub(crate) struct VideoEncoderSpec {
    pub(crate) codec: Codec,
    /// FFmpeg encoder name (e.g. `libx264`, `h264_nvenc`).
    pub(crate) name: String,
    pub(crate) hardware: bool,
    pub(crate) fallback_reason: Option<String>,
}

/// Look up a named encoder, requiring it to be a usable video encoder.
fn lookup_video_encoder(name: &str) -> Option<Codec> {
    encoder::find_by_name(name).filter(|c| c.is_encoder() && c.is_video())
}

/// Software H.264 encoder (libx264 when present, otherwise ffmpeg's default).
pub(crate) fn software_h264() -> Option<Codec> {
    lookup_video_encoder("libx264")
        .or_else(|| encoder::find(codec::Id::H264).filter(|c| c.is_encoder() && c.is_video()))
}

fn fallback_to_software(
    requested: &str,
    reason: &str,
) -> std::result::Result<VideoEncoderSpec, FrameForgeError> {
    let codec = software_h264().ok_or_else(|| {
        FrameForgeError::Export(format!(
            "requested encoder '{requested}' is unavailable ({reason}) and no software \
             H.264 encoder is present"
        ))
    })?;
    Ok(VideoEncoderSpec {
        codec,
        name: codec.name().to_string(),
        hardware: false,
        fallback_reason: Some(format!(
            "'{requested}' unavailable ({reason}); falling back to {} (software)",
            codec.name()
        )),
    })
}

/// Hardware offload can be disabled for troubleshooting.
fn hw_disabled() -> bool {
    std::env::var("FRAMEFORGE_DISABLE_HW")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Map the UI encoder preference onto a concrete FFmpeg encoder.
///
/// Hardware preferences (`vaapi`, `nvenc`) are honoured only when the runtime
/// actually provides them; otherwise the export continues in software and the
/// reason is reported through [`EncoderReport`].
pub(crate) fn resolve_video_encoder(
    pref: &str,
) -> std::result::Result<VideoEncoderSpec, FrameForgeError> {
    let pref = pref.trim().to_lowercase();
    let hw_off = hw_disabled();

    match pref.as_str() {
        "nvenc" | "h264_nvenc" => {
            if !hw_off {
                if let Some(codec) = lookup_video_encoder("h264_nvenc") {
                    return Ok(VideoEncoderSpec {
                        codec,
                        name: "h264_nvenc".into(),
                        hardware: true,
                        fallback_reason: None,
                    });
                }
                return fallback_to_software("nvenc", "h264_nvenc is not compiled into ffmpeg");
            }
            fallback_to_software("nvenc", "disabled by FRAMEFORGE_DISABLE_HW")
        }
        "vaapi" | "h264_vaapi" => {
            if hw_off {
                return fallback_to_software("vaapi", "disabled by FRAMEFORGE_DISABLE_HW");
            }
            if !vaapi::device_available() {
                return fallback_to_software("vaapi", "no usable VAAPI device");
            }
            match lookup_video_encoder("h264_vaapi") {
                Some(codec) => Ok(VideoEncoderSpec {
                    codec,
                    name: "h264_vaapi".into(),
                    hardware: true,
                    fallback_reason: None,
                }),
                None => fallback_to_software("vaapi", "h264_vaapi is not compiled into ffmpeg"),
            }
        }
        "x265" | "h265" | "hevc" => match lookup_video_encoder("libx265") {
            Some(codec) => Ok(VideoEncoderSpec {
                codec,
                name: "libx265".into(),
                hardware: false,
                fallback_reason: None,
            }),
            None => fallback_to_software("x265", "libx265 is not compiled into ffmpeg"),
        },
        "svtav1" | "av1" => match lookup_video_encoder("libsvtav1") {
            Some(codec) => Ok(VideoEncoderSpec {
                codec,
                name: "libsvtav1".into(),
                hardware: false,
                fallback_reason: None,
            }),
            None => fallback_to_software("svtav1", "libsvtav1 is not compiled into ffmpeg"),
        },
        _ => match software_h264() {
            Some(codec) => Ok(VideoEncoderSpec {
                name: codec.name().to_string(),
                codec,
                hardware: false,
                fallback_reason: None,
            }),
            None => Err(FrameForgeError::Export("no H.264 encoder available".into())),
        },
    }
}

/// CRF target for the quality preset.
pub(crate) fn quality_crf(quality_preset: &str) -> &'static str {
    match quality_preset {
        "high" => "18",
        "medium" => "23",
        "low" => "28",
        _ => "23",
    }
}

/// Software encoder speed/quality preset.
pub(crate) fn software_preset(quality_preset: &str) -> &'static str {
    match quality_preset {
        "ultrafast" => "ultrafast",
        "superfast" => "superfast",
        "veryfast" => "veryfast",
        "faster" => "faster",
        "fast" => "fast",
        "slow" => "slow",
        "slower" => "slower",
        "veryslow" => "veryslow",
        "high" => "veryslow",
        "low" => "fast",
        _ => "medium",
    }
}

/// NVENC preset (p1..p7 are not available in older ffmpeg builds).
fn nvenc_preset(quality_preset: &str) -> &'static str {
    match quality_preset {
        "ultrafast" | "superfast" | "veryfast" | "faster" | "fast" | "low" => "fast",
        "slow" | "slower" | "veryslow" | "high" => "slow",
        _ => "medium",
    }
}

/// Build the private options for an opened video encoder.
///
/// Software encoders get `threads=0`, which lets libavcodec use every core.
/// Without it libavcodec defaults to a single thread, which is the main reason
/// a software export feels far slower than the equivalent `ffmpeg` command.
fn video_encoder_options(spec: &VideoEncoderSpec, quality_preset: &str) -> Dictionary<'static> {
    let mut opts = Dictionary::new();

    if spec.hardware {
        if spec.name.ends_with("nvenc") {
            opts.set("preset", nvenc_preset(quality_preset));
            opts.set("rc", "vbr");
            opts.set("cq", quality_crf(quality_preset));
            opts.set("bf", "2");
            opts.set("delay", "0");
        }
        // VAAPI quality options vary per driver; leave them at their defaults
        // rather than passing options the driver may reject.
        return opts;
    }

    opts.set("threads", "0");
    opts.set("preset", software_preset(quality_preset));
    opts.set("crf", quality_crf(quality_preset));
    opts
}

/// Geometry / timing parameters for an output video encoder.
pub(crate) struct VideoEncoderParams {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) sw_format: ffmpeg_next::format::Pixel,
    pub(crate) frame_rate: ffmpeg_next::Rational,
    pub(crate) time_base: ffmpeg_next::Rational,
    /// Force a B-frame count for software encoders (`None` = encoder default).
    /// Smart rendering uses `Some(0)` for boundary GOPs so PTS == DTS and the
    /// join with copied B-frame GOPs stays monotonic.
    pub(crate) bframes: Option<i32>,
    /// Source H.264 profile name (baseline/main/high...), when known.
    pub(crate) profile: Option<String>,
    /// Source H.264 level (e.g. 30 for 3.0), when known.
    pub(crate) level: Option<u32>,
    /// Source x264 parameter string (from the SEI), when available.
    pub(crate) x264_params: Option<String>,
    /// Source reference-frame count (from the SPS/decoder).
    pub(crate) refs: Option<usize>,
    /// Source GOP size (keyframe interval) for the `keyint` encoder option.
    pub(crate) gop_size: Option<u32>,
    /// Emit container-level global header (avcC) instead of in-band SPS/PPS.
    pub(crate) global_header: bool,
}

/// Open a video encoder for one input stream.
///
/// When `spec` is the VAAPI encoder the hardware frames context is attached
/// before the codec is opened and the encoder is opened with the VAAPI pixel
/// format; the returned frames context is then used to upload each frame.
pub(crate) fn open_video_encoder(
    mut ctx: codec::Context,
    params: &VideoEncoderParams,
    spec: &VideoEncoderSpec,
    quality_preset: &str,
) -> std::result::Result<(encoder::Video, Option<vaapi::VaapiFrames>), FrameForgeError> {
    let wants_vaapi = spec.hardware && spec.name == "h264_vaapi";

    let hw = if wants_vaapi {
        Some(
            unsafe {
                vaapi::VaapiFrames::attach(&mut ctx, params.width, params.height, params.sw_format)
            }
            .map_err(|e| FrameForgeError::Export(format!("VAAPI setup failed: {e}")))?,
        )
    } else {
        None
    };

    let mut enc = ctx
        .encoder()
        .video()
        .map_err(|e| FrameForgeError::Export(format!("encoder video failed: {e}")))?;
    enc.set_width(params.width);
    enc.set_height(params.height);
    if wants_vaapi {
        enc.set_format(ffmpeg_next::format::Pixel::from(
            ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
        ));
    } else {
        enc.set_format(params.sw_format);
    }
    enc.set_frame_rate(Some(params.frame_rate));
    enc.set_time_base(params.time_base);
    if params.global_header {
        // MP4/MOV need the SPS/PPS in the container's `avcC`; without this flag
        // libx264 leaves `extradata` empty and smart rendering cannot decide
        // whether in-band parameter sets are required.
        enc.set_flags(ffmpeg_next::codec::Flags::GLOBAL_HEADER);
    }

    let mut opts = video_encoder_options(spec, quality_preset);
    if !spec.hardware {
        if let Some(bf) = params.bframes {
            let bf = bf.to_string();
            opts.set("bf", &bf);
        }
        if let Some(profile) = &params.profile {
            opts.set("profile", profile);
        }
        if let Some(level) = params.level {
            let level = level.to_string();
            opts.set("level", &level);
        }
        if let Some(refs) = params.refs {
            let refs = refs.to_string();
            opts.set("refs", &refs);
        }
        if let Some(gop) = params.gop_size {
            let gop = gop.to_string();
            opts.set("g", &gop);
            opts.set("keyint", &gop);
            opts.set("keyint-min", &gop);
            opts.set("g", &gop);
            opts.set("keyint", &gop);
            opts.set("keyint-min", &gop);
        }
        if let Some(x264_params) = &params.x264_params {
            // Pass x264 params via x264-params (single source of truth).
            opts.set("x264-params", x264_params);
            // Mirror the weighted-prediction setting onto the dedicated ffmpeg
            // option as well: x264-params alone is not always honoured by every
            // libx264/ffmpeg build, which previously left weighted pred off and
            // produced a PPS that differed from the source.
            for part in x264_params.split(':') {
                if let Some((k, v)) = part.split_once('=') {
                    if k == "weightp" {
                        opts.set("weightp", v);
                    } else if k == "8x8dct" {
                        opts.set("8x8dct", v);
                    }
                }
            }
        }
    }

    let opened = enc
        .open_with(opts)
        .map_err(|e| FrameForgeError::Export(format!("encoder open failed: {e}")))?;

    Ok((opened, hw))
}

/// Select an audio encoder by preference name, falling back to AAC.
pub(crate) fn select_audio_encoder(pref: &str) -> std::result::Result<Codec, FrameForgeError> {
    let preferred = match pref {
        "opus" => Some("libopus"),
        "mp3" => Some("libmp3lame"),
        _ => None,
    };
    if let Some(name) = preferred {
        if let Some(codec) = encoder::find_by_name(name).filter(|c| c.is_encoder() && c.is_audio())
        {
            return Ok(codec);
        }
    }
    encoder::find(codec::Id::AAC)
        .filter(|c| c.is_encoder())
        .ok_or_else(|| FrameForgeError::Export("no AAC audio encoder available".into()))
}

/// Normalize video frame: scale/pix_fmt via swscale.
pub(crate) fn normalize_video_frame(
    frame: &ffmpeg_next::util::frame::Video,
    target_width: Option<u32>,
    target_height: Option<u32>,
    target_pix_fmt: Option<&str>,
) -> Result<ffmpeg_next::util::frame::Video> {
    let src_w = frame.width();
    let src_h = frame.height();
    let src_fmt = frame.format();

    let dst_w = target_width.unwrap_or(src_w);
    let dst_h = target_height.unwrap_or(src_h);
    let dst_fmt = target_pix_fmt
        .and_then(parse_pixel_format)
        .unwrap_or(src_fmt);

    if dst_w == src_w && dst_h == src_h && dst_fmt == src_fmt {
        return Ok(frame.clone());
    }

    let mut sws = ffmpeg_next::software::scaling::Context::get(
        src_fmt,
        src_w,
        src_h,
        dst_fmt,
        dst_w,
        dst_h,
        ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
    )
    .map_err(|_| FrameForgeError::Export("swscale context creation failed".into()))?;

    let mut dst = ffmpeg_next::util::frame::Video::new(dst_fmt, dst_w, dst_h);
    sws.run(frame, &mut dst)
        .map_err(|e| FrameForgeError::Export(format!("swscale run failed: {e}")))?;

    Ok(dst)
}

/// Parse pixel format string to ffmpeg Pixel enum.
pub(crate) fn parse_pixel_format(s: &str) -> Option<ffmpeg_next::format::Pixel> {
    match s.to_lowercase().as_str() {
        "yuv420p" => Some(ffmpeg_next::format::Pixel::YUV420P),
        "yuv422p" => Some(ffmpeg_next::format::Pixel::YUV422P),
        "yuv444p" => Some(ffmpeg_next::format::Pixel::YUV444P),
        "yuv410p" => Some(ffmpeg_next::format::Pixel::YUV410P),
        "yuv411p" => Some(ffmpeg_next::format::Pixel::YUV411P),
        "yuv420p9le" => Some(ffmpeg_next::format::Pixel::YUV420P9LE),
        "yuv420p10le" => Some(ffmpeg_next::format::Pixel::YUV420P10LE),
        "yuv420p12le" => Some(ffmpeg_next::format::Pixel::YUV420P12LE),
        "yuv420p14le" => Some(ffmpeg_next::format::Pixel::YUV420P14LE),
        "yuv420p16le" => Some(ffmpeg_next::format::Pixel::YUV420P16LE),
        "yuv422p9le" => Some(ffmpeg_next::format::Pixel::YUV422P9LE),
        "yuv422p10le" => Some(ffmpeg_next::format::Pixel::YUV422P10LE),
        "yuv422p12le" => Some(ffmpeg_next::format::Pixel::YUV422P12LE),
        "yuv422p14le" => Some(ffmpeg_next::format::Pixel::YUV422P14LE),
        "yuv422p16le" => Some(ffmpeg_next::format::Pixel::YUV422P16LE),
        "yuv444p9le" => Some(ffmpeg_next::format::Pixel::YUV444P9LE),
        "yuv444p10le" => Some(ffmpeg_next::format::Pixel::YUV444P10LE),
        "yuv444p12le" => Some(ffmpeg_next::format::Pixel::YUV444P12LE),
        "yuv444p14le" => Some(ffmpeg_next::format::Pixel::YUV444P14LE),
        "yuv444p16le" => Some(ffmpeg_next::format::Pixel::YUV444P16LE),
        "nv12" => Some(ffmpeg_next::format::Pixel::NV12),
        "nv21" => Some(ffmpeg_next::format::Pixel::NV21),
        "nv16" => Some(ffmpeg_next::format::Pixel::NV16),
        "p010le" => Some(ffmpeg_next::format::Pixel::P010LE),
        "p016le" => Some(ffmpeg_next::format::Pixel::P016LE),
        "rgba" => Some(ffmpeg_next::format::Pixel::RGBA),
        "bgra" => Some(ffmpeg_next::format::Pixel::BGRA),
        "argb" => Some(ffmpeg_next::format::Pixel::ARGB),
        "abgr" => Some(ffmpeg_next::format::Pixel::ABGR),
        "rgb24" => Some(ffmpeg_next::format::Pixel::RGB24),
        "bgr24" => Some(ffmpeg_next::format::Pixel::BGR24),
        "rgb48le" => Some(ffmpeg_next::format::Pixel::RGB48LE),
        "rgb48be" => Some(ffmpeg_next::format::Pixel::RGB48BE),
        "bgr48le" => Some(ffmpeg_next::format::Pixel::BGR48LE),
        "bgr48be" => Some(ffmpeg_next::format::Pixel::BGR48BE),
        "rgba64le" => Some(ffmpeg_next::format::Pixel::RGBA64LE),
        "rgba64be" => Some(ffmpeg_next::format::Pixel::RGBA64BE),
        "bgra64le" => Some(ffmpeg_next::format::Pixel::BGRA64LE),
        "bgra64be" => Some(ffmpeg_next::format::Pixel::BGRA64BE),
        "gray8" => Some(ffmpeg_next::format::Pixel::GRAY8),
        "gray9le" => Some(ffmpeg_next::format::Pixel::GRAY9LE),
        "gray10le" => Some(ffmpeg_next::format::Pixel::GRAY10LE),
        "gray12le" => Some(ffmpeg_next::format::Pixel::GRAY12LE),
        "gray14le" => Some(ffmpeg_next::format::Pixel::GRAY14LE),
        "gray16le" => Some(ffmpeg_next::format::Pixel::GRAY16LE),
        "gray16be" => Some(ffmpeg_next::format::Pixel::GRAY16BE),
        "yuyv422" => Some(ffmpeg_next::format::Pixel::YUYV422),
        "uyvy422" => Some(ffmpeg_next::format::Pixel::UYVY422),
        "vyuy422" => Some(ffmpeg_next::format::Pixel::YVYU422),
        "yuva420p" => Some(ffmpeg_next::format::Pixel::YUVA420P),
        "yuva422p" => Some(ffmpeg_next::format::Pixel::YUVA422P),
        "yuva444p" => Some(ffmpeg_next::format::Pixel::YUVA444P),
        "gbrp" => Some(ffmpeg_next::format::Pixel::GBRP),
        "gbrp9le" => Some(ffmpeg_next::format::Pixel::GBRP9LE),
        "gbrp10le" => Some(ffmpeg_next::format::Pixel::GBRP10LE),
        "gbrp12le" => Some(ffmpeg_next::format::Pixel::GBRP12LE),
        "gbrp14le" => Some(ffmpeg_next::format::Pixel::GBRP14LE),
        "gbrp16le" => Some(ffmpeg_next::format::Pixel::GBRP16LE),
        "gbrap" => Some(ffmpeg_next::format::Pixel::GBRAP),
        "gbrap10le" => Some(ffmpeg_next::format::Pixel::GBRAP10LE),
        "gbrap12le" => Some(ffmpeg_next::format::Pixel::GBRAP12LE),
        "gbrap16le" => Some(ffmpeg_next::format::Pixel::GBRAP16LE),
        _ => None,
    }
}

/// Pre-check free space at destination; returns bytes free or error.
pub fn disk_space_free(destination: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path = destination.parent().unwrap_or(destination);
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|e| FrameForgeError::Export(format!("Invalid path: {}", e)))?;

        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };

        if result != 0 {
            return Err(FrameForgeError::Export(format!(
                "statvfs failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        // Free space = f_bavail * f_frsize (available blocks * fragment size)
        let free_bytes = stat.f_bavail as u64 * stat.f_frsize as u64;
        Ok(free_bytes)
    }
    #[cfg(not(unix))]
    {
        Err(FrameForgeError::Export(
            "disk_space_free only implemented on unix".into(),
        ))
    }
}

/// Ensure the destination has enough free space for an estimated output.
pub fn ensure_disk_space(destination: &Path, estimated_bytes: u64) -> Result<()> {
    let free = disk_space_free(destination)?;
    if free < estimated_bytes {
        return Err(FrameForgeError::Export(format!(
            "insufficient disk space: need ~{} bytes, have {} bytes",
            estimated_bytes, free
        )));
    }
    Ok(())
}

/// Estimate the output size as bitrate x duration + 10% margin.
pub fn estimate_output_bytes(
    plan: &ExportPlan,
    clip_map: &HashMap<SourceId, PathBuf>,
) -> Result<u64> {
    let mut total = 0u64;
    for segment in &plan.segments {
        let path = clip_map.get(&segment.source_id).ok_or_else(|| {
            FrameForgeError::Export(format!(
                "clip not found for source_id: {}",
                segment.source_id.0
            ))
        })?;
        let info = frameforge_media_probe::probe(path)?;
        let duration = (segment.range.end.to_seconds() - segment.range.start.to_seconds()).max(0.0);
        let bitrate = info.format.bit_rate.unwrap_or(0) as f64;
        total += (bitrate * duration / 8.0) as u64;
    }
    Ok(total + total / 10)
}

/// Determine the FFmpeg output muxer name from a file extension.
pub fn output_format_from_extension(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("mp4") | Some("m4v") => Some("mp4"),
        Some("mkv") => Some("matroska"),
        Some("ts") => Some("mpegts"),
        Some("mov") => Some("mov"),
        Some("webm") => Some("webm"),
        Some("avi") => Some("avi"),
        Some("flv") => Some("flv"),
        Some("wmv") => Some("asf"),
        Some("ogv") | Some("ogg") => Some("ogg"),
        _ => None,
    }
}

/// Write output atomically: write to temp file, then rename.
pub fn atomic_write<F>(destination: &Path, mut write: F) -> Result<()>
where
    F: FnMut(&Path, Option<&'static str>) -> Result<()>,
{
    // Ensure the destination directory exists.
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| FrameForgeError::Io(e))?;
        }
    }

    // Use a unique temp file name to avoid collisions between parallel exports
    // to the same destination.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = destination.with_file_name(format!(
        ".{}.{}.tmp",
        destination.file_name().unwrap().to_string_lossy(),
        unique
    ));

    // RAII guard: removes the temp file on any failure, cancel, or panic.
    struct TempGuard {
        path: PathBuf,
        armed: bool,
    }
    impl Drop for TempGuard {
        fn drop(&mut self) {
            if self.armed {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    let mut guard = TempGuard {
        path: temp.clone(),
        armed: true,
    };

    let fmt = output_format_from_extension(destination).or(Some("mp4"));
    write(&temp, fmt)?;
    std::fs::rename(&temp, destination)?;
    guard.armed = false;
    Ok(())
}

/// Context for remuxing: holds all per-stream state.
struct RemuxCtx<'a> {
    ictx: &'a mut ffmpeg_next::format::context::Input,
    octx: &'a mut ffmpeg_next::format::context::Output,
    stream_mapping: &'a [i32],
    base_offsets: &'a mut [i64],
    base_dts_offsets: &'a mut [i64],
    last_dts: &'a mut [Option<i64>],
    /// Per-stream segment start PTS (input timebase) for independent PTS rebasing.
    stream_seg_start_pts: &'a mut [Option<i64>],
    /// Per-stream segment start DTS (input timebase) for independent DTS rebasing.
    stream_seg_start_dts: &'a mut [Option<i64>],
    video_stream_idx: Option<usize>,
}

/// Remux a single segment from input to output with timestamp rebasing.
fn remux_segment(
    ctx: &mut RemuxCtx<'_>,
    segment: &PlannedSegment,
    cancel: &CancelToken,
) -> Result<()> {
    let _source_id = segment.source_id.0 as usize;

    // Reset per-stream segment start for this segment so PTS rebasing is not
    // carried over from a previous segment.
    ctx.stream_seg_start_pts.iter_mut().for_each(|s| *s = None);
    ctx.stream_seg_start_dts.iter_mut().for_each(|s| *s = None);
    // Keep per-stream last DTS across segments so the monotonic guard can
    // still prevent equal/non-increasing DTS at segment boundaries; base offsets
    // are advanced per segment, so legitimate packets stay strictly increasing.

    let seg_start_seconds = segment
        .cut_points
        .first()
        .map(|t| t.to_seconds())
        .unwrap_or_else(|| segment.range.start.to_seconds());
    let seg_end_seconds = segment
        .cut_points
        .get(1)
        .map(|t| t.to_seconds())
        .unwrap_or_else(|| segment.range.end.to_seconds());

    let start_offset_seconds = ctx
        .ictx
        .streams()
        .find(|s| s.parameters().medium() == media::Type::Video)
        .map(|s| {
            let st = s.start_time();
            if st == i64::MIN {
                0.0
            } else {
                st as f64 * (s.time_base().0 as f64 / s.time_base().1 as f64)
            }
        })
        .unwrap_or(0.0);

    let seek_target_seconds = start_offset_seconds + seg_start_seconds;
    let start_pts_av = (seek_target_seconds * 1_000_000.0) as i64;

    if let Some(_video_idx) = ctx.video_stream_idx {
        ctx.ictx
            .seek(start_pts_av, i64::MIN..start_pts_av)
            .map_err(|e| FrameForgeError::Export(format!("seek failed: {e}")))?;
    }

    let mut stream_done: Vec<bool> = vec![false; ctx.stream_mapping.len()];
    let mut stream_written: Vec<bool> = vec![false; ctx.base_offsets.len()];
    let seg_base_pts: Vec<i64> = ctx.base_offsets.to_vec();
    let seg_base_dts: Vec<i64> = ctx.base_dts_offsets.to_vec();
    let mut last_final_pts: Vec<Option<i64>> = vec![None; ctx.base_offsets.len()];
    let mut last_final_dts: Vec<Option<i64>> = vec![None; ctx.base_offsets.len()];

    for (stream, mut packet) in ctx.ictx.packets() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }

        let ist_index = stream.index() as i32;
        let ost_index = if (ist_index as usize) < ctx.stream_mapping.len() {
            ctx.stream_mapping[ist_index as usize]
        } else {
            -1
        };
        if ost_index < 0 {
            continue;
        }

        let ist_tb = stream.time_base();
        let tb_sec = ist_tb.0 as f64 / ist_tb.1 as f64;

        let pkt_pts = packet.pts().unwrap_or(0);
        let pkt_seconds = pkt_pts as f64 * tb_sec - start_offset_seconds;

        if pkt_seconds < seg_start_seconds - 1e-9 {
            continue;
        }
        // End boundary is exclusive: stop once a packet reaches (or is within
        // floating-point epsilon of) the segment end.
        if pkt_seconds + 1e-9 >= seg_end_seconds {
            if (ist_index as usize) < stream_done.len() {
                stream_done[ist_index as usize] = true;
            }
            // Stop only when every selected stream has reached its end.
            let all_done = ctx
                .stream_mapping
                .iter()
                .enumerate()
                .all(|(i, ost)| *ost < 0 || stream_done.get(i).copied().unwrap_or(true));
            if all_done {
                break;
            }
            continue;
        }

        // Use the segment start (in input timebase units) as the reference for
        // both PTS and DTS.  Deriving it from the first packet is unsafe because
        // the first packet after a seek may be a B-frame whose PTS is not the
        // segment start, which corrupts every subsequent timestamp.
        // Capture per-stream first-packet DTS (for B-frame-heavy GOPs the first
        // packet in decode order has the smallest DTS, which may precede the
        // segment start).
        if ctx.stream_seg_start_dts[ist_index as usize].is_none() {
            ctx.stream_seg_start_dts[ist_index as usize] = packet.dts().or(Some(pkt_pts));
        }
        let seg_start_pts = ((start_offset_seconds + seg_start_seconds) / tb_sec).round() as i64;
        let stream_start_pts = seg_start_pts;
        let stream_start_dts = ctx.stream_seg_start_dts[ist_index as usize].unwrap_or(pkt_pts);

        // Calculate relative PTS/DTS in input timebase units (segment starts at 0).
        let relative_pts = pkt_pts - stream_start_pts;
        let relative_dts = packet.dts().unwrap_or(pkt_pts) - stream_start_dts;

        packet.set_pts(Some(relative_pts));
        packet.set_dts(Some(relative_dts));

        let ost = ctx
            .octx
            .stream(ost_index as usize)
            .ok_or_else(|| FrameForgeError::Export("output stream not found".into()))?;
        let out_tb = ost.time_base();
        // Rescale from input timebase to output timebase.
        packet.rescale_ts(ist_tb, out_tb);
        packet.set_position(-1);
        packet.set_stream(ost_index as usize);

        // Apply output offset in output timebase units. This keeps the timeline
        // continuous across segments without mixing timebase units.
        let pkt_pts_after = packet.pts().unwrap_or(0);
        let pkt_dts_after = packet.dts().unwrap_or(0);

        let base_pts = seg_base_pts[ost_index as usize];
        let base_dts = seg_base_dts[ost_index as usize];

        let mut adjusted_pts = pkt_pts_after + base_pts;
        let mut adjusted_dts = pkt_dts_after + base_dts;

        // B-frames can have a decode timestamp that precedes the segment start
        // (their PTS is in range but DTS references an earlier GOP). Clamp the
        // DTS to the segment base so the stream never moves backwards.
        if adjusted_dts < base_dts {
            adjusted_dts = base_dts;
        }

        // Enforce strictly increasing DTS within this segment.
        let last = ctx.last_dts[ost_index as usize].unwrap_or(i64::MIN);
        if adjusted_dts <= last {
            adjusted_dts = last + 1;
        }
        ctx.last_dts[ost_index as usize] = Some(adjusted_dts);

        // Do not force PTS >= DTS: B-frames legitimately have PTS < DTS.
        // Never let PTS fall below the segment base.
        if adjusted_pts < base_pts {
            adjusted_pts = base_pts;
        }

        packet.set_pts(Some(adjusted_pts));
        packet.set_dts(Some(adjusted_dts));

        stream_written[ost_index as usize] = true;
        // Track the maximum PTS/DTS seen in this segment so the next segment
        // starts strictly after all packets of this one (handles B-frames and
        // out-of-order DTS within a segment).
        let final_pts = packet.pts().unwrap_or(adjusted_pts);
        let final_dts = packet.dts().unwrap_or(adjusted_dts);

        let prev_pts = last_final_pts[ost_index as usize].unwrap_or(final_pts);
        if final_pts > prev_pts {
            last_final_pts[ost_index as usize] = Some(final_pts);
        } else if last_final_pts[ost_index as usize].is_none() {
            last_final_pts[ost_index as usize] = Some(final_pts);
        }
        let prev_dts = last_final_dts[ost_index as usize].unwrap_or(final_dts);
        if final_dts > prev_dts {
            last_final_dts[ost_index as usize] = Some(final_dts);
        } else if last_final_dts[ost_index as usize].is_none() {
            last_final_dts[ost_index as usize] = Some(final_dts);
        }

        packet.write_interleaved(ctx.octx).map_err(|e| {
            FrameForgeError::Export(format!(
                "write_interleaved failed: {e} (ost={}, adjusted_pts={}, adjusted_dts={})",
                ost_index, adjusted_pts, adjusted_dts
            ))
        })?;
    }

    // Advance base offsets to the last written PTS/DTS of this segment (+1) so the
    // next segment starts strictly after this one.
    for (i, p) in last_final_pts
        .iter()
        .enumerate()
        .take(ctx.base_offsets.len())
    {
        if let Some(p) = p {
            ctx.base_offsets[i] = p + 1;
        }
    }
    for (i, d) in last_final_dts
        .iter()
        .enumerate()
        .take(ctx.base_dts_offsets.len())
    {
        if let Some(d) = d {
            ctx.base_dts_offsets[i] = d + 1;
            eprintln!(
                "DEBUG seg end: stream {} last_final_dts={} new_base={}",
                i, d, ctx.base_dts_offsets[i]
            );
        } else {
            eprintln!(
                "DEBUG seg end: stream {} last_final_dts=None base unchanged={}",
                i, ctx.base_dts_offsets[i]
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_works() {
        let c = CancelToken::new();
        assert!(!c.is_cancelled());
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[test]
    fn atomic_write_creates_temp_and_renames() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let dest = dir.join(format!("frameforge_test_{}.mp4", std::process::id()));

        let _ = std::fs::remove_file(&dest);

        let result = atomic_write(&dest, |path, _fmt| {
            let mut f = std::fs::File::create(path).map_err(|e| FrameForgeError::Io(e))?;
            f.write_all(b"test data")
                .map_err(|e| FrameForgeError::Io(e))?;
            Ok(())
        });

        assert!(result.is_ok());
        assert!(dest.exists());

        let content = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(content, "test data");

        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn disk_space_returns_positive() {
        let free = disk_space_free(Path::new("/")).unwrap();
        assert!(free > 0);
    }

    #[test]
    fn output_format_detection() {
        use std::path::Path;
        assert_eq!(
            output_format_from_extension(Path::new("test.mp4")),
            Some("mp4")
        );
        assert_eq!(
            output_format_from_extension(Path::new("test.mkv")),
            Some("matroska")
        );
        assert_eq!(
            output_format_from_extension(Path::new("test.ts")),
            Some("mpegts")
        );
        assert_eq!(
            output_format_from_extension(Path::new("test.mov")),
            Some("mov")
        );
        assert_eq!(
            output_format_from_extension(Path::new("test.webm")),
            Some("webm")
        );
        assert_eq!(
            output_format_from_extension(Path::new("test.avi")),
            Some("avi")
        );
        assert_eq!(
            output_format_from_extension(Path::new("test.unknown")),
            None
        );
    }

    fn opts_to_map(opts: &Dictionary) -> HashMap<String, String> {
        opts.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn software_options_enable_all_cores_and_preset() {
        let spec = VideoEncoderSpec {
            codec: software_h264().expect("libx264 or default h264 encoder"),
            name: "libx264".into(),
            hardware: false,
            fallback_reason: None,
        };

        let map = opts_to_map(&video_encoder_options(&spec, "fast"));

        // thread_count=0 => libavcodec auto-detects the core count. This is the
        // fix for single-threaded software exports.
        assert_eq!(map.get("threads").map(String::as_str), Some("0"));
        assert_eq!(map.get("preset").map(String::as_str), Some("fast"));
        assert_eq!(map.get("crf").map(String::as_str), Some("23"));
    }

    #[test]
    fn quality_preset_maps_to_crf_and_speed() {
        assert_eq!(quality_crf("high"), "18");
        assert_eq!(quality_crf("medium"), "23");
        assert_eq!(quality_crf("low"), "28");
        assert_eq!(quality_crf("unknown"), "23");

        assert_eq!(software_preset("high"), "veryslow");
        assert_eq!(software_preset("low"), "fast");
        assert_eq!(software_preset("fast"), "fast");
        assert_eq!(software_preset("nonsense"), "medium");

        assert_eq!(nvenc_preset("high"), "slow");
        assert_eq!(nvenc_preset("fast"), "fast");
        assert_eq!(nvenc_preset("medium"), "medium");
    }

    #[test]
    fn nvenc_options_do_not_pass_x264_private_keys() {
        let spec = VideoEncoderSpec {
            codec: software_h264().expect("encoder present"),
            name: "h264_nvenc".into(),
            hardware: true,
            fallback_reason: None,
        };

        let map = opts_to_map(&video_encoder_options(&spec, "high"));

        assert_eq!(map.get("preset").map(String::as_str), Some("slow"));
        assert_eq!(map.get("rc").map(String::as_str), Some("vbr"));
        assert!(map.contains_key("cq"));
        // x264-only options must not leak into an nvenc open, or avcodec_open2
        // would reject the dictionary.
        assert!(!map.contains_key("crf"));
        assert!(!map.contains_key("threads"));
    }

    #[test]
    fn vaapi_options_are_left_empty() {
        let spec = VideoEncoderSpec {
            codec: software_h264().expect("encoder present"),
            name: "h264_vaapi".into(),
            hardware: true,
            fallback_reason: None,
        };

        // Driver-specific quality options are intentionally not set.
        assert!(video_encoder_options(&spec, "fast").iter().next().is_none());
    }

    #[test]
    fn unknown_preference_falls_back_to_software_h264() {
        // `FRAMEFORGE_DISABLE_HW` must not affect a plain software preference.
        let spec = resolve_video_encoder("x264").expect("software h264 available");
        assert!(!spec.hardware);
        assert!(spec.fallback_reason.is_none());
    }

    #[test]
    fn vaapi_preference_never_reports_fake_hardware() {
        match resolve_video_encoder("vaapi") {
            Ok(spec) => {
                if spec.hardware {
                    assert_eq!(spec.name, "h264_vaapi");
                    assert!(spec.fallback_reason.is_none());
                } else {
                    // No usable device/encoder: must be a software fallback with
                    // an explicit reason, never a silent software stand-in.
                    assert!(spec.fallback_reason.is_some());
                }
            }
            Err(e) => panic!("vaapi preference should degrade, got error: {e}"),
        }
    }
}
