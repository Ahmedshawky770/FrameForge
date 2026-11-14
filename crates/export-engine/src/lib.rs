//! export-engine: stream-copy remux + re-encode pipeline orchestration.
//!
//! Atomic output (temp file + rename), disk-space precheck, progress, cancellation.
//!
//! Stream-copy uses ffmpeg-next directly (packet-by-packet remux), never loading
//! whole files into RAM. Re-encode uses decode → normalize → encode pipeline.

use ffmpeg_next::{codec, encoder, media, packet, Dictionary, Rational as FfmpegRational};
use frameforge_core_types::{Clip, FrameForgeError, PathBuf, Result, SourceId};
use frameforge_edit_engine::{ExportPlan, PlannedSegment};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub type ProgressCallback = dyn Fn(f32) + Send + Sync;

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
        Self {
            clips,
            destination,
        }
    }

    fn build_clip_map(&self) -> HashMap<SourceId, PathBuf> {
        self.clips
            .iter()
            .map(|c| (c.id, c.path.clone()))
            .collect()
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

impl StreamCopyExecutor {
}

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
        segments_by_source.entry(segment.source_id).or_default().push(segment);
    }

    // Create output context first
    let mut octx = match output_format {
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
    let first_clip_path = clip_map.values().next()
        .ok_or_else(|| FrameForgeError::Export("no clips in map".into()))?;
    
    let first_ictx = ffmpeg_next::format::input(first_clip_path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

    let input_streams: Vec<ffmpeg_next::Stream> = first_ictx.streams().collect();
    let mut stream_mapping = vec![-1; input_streams.len()];
    let mut ist_time_bases = vec![FfmpegRational(1, 1); input_streams.len()];

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
        ist_time_bases[ist_index] = ist.time_base();
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
    octx.write_header()
        .map_err(|e| FrameForgeError::Export(format!("write_header failed: {e}")))?;

    let mut base_offsets: Vec<i64> = vec![0; ost_index];
    let mut last_dts: Vec<Option<i64>> = vec![None; ost_index];
    let mut stream_seg_start: Vec<Option<i64>> = vec![None; input_streams.len()];

    let total_segments = plan.segments.len() as f32;

    // Group segments by source file path to open each file only once.
    let mut segments_by_path: HashMap<&Path, Vec<&PlannedSegment>> = HashMap::new();
    for segment in &plan.segments {
        let path = clip_map.get(&segment.source_id)
            .expect("clip source_id must map to a path");
        segments_by_path.entry(path).or_default().push(segment);
    }

    // Process each unique source file once.
    for (file_path, file_segments) in &segments_by_path {
        // Open input file context once per source.
        let mut ictx = ffmpeg_next::format::input(file_path)
            .map_err(|e| FrameForgeError::Probe(format!("format::input failed for {}: {e}", file_path.display())))?;

        // Reset per-stream segment start.
        stream_seg_start.iter_mut().for_each(|s| *s = None);

        let mut ctx = RemuxCtx {
            ictx: &mut ictx,
            octx: &mut octx,
            stream_mapping: &stream_mapping,
            ist_time_bases: &ist_time_bases,
            base_offsets: &mut base_offsets,
            last_dts: &mut last_dts,
            stream_seg_start: &mut stream_seg_start,
            video_stream_idx,
        };

        // Process all segments from this file through the shared context.
        let mut seg_idx_in_file = 0usize;
        for segment in file_segments {
            if cancel.is_cancelled() {
                return Err(FrameForgeError::Export("export cancelled".into()));
            }

            remux_segment(&mut ctx, segment, cancel)?;

            // Report progress after each segment (still report per-segment for UI).
            progress((seg_idx_in_file + 1) as f32 / total_segments);
            seg_idx_in_file += 1;
        }
        // Input context is dropped here, closing the file after all segments processed.
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
        self.clips
            .iter()
            .map(|c| (c.id, c.path.clone()))
            .collect()
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
        let clip_map = self.build_clip_map();

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
    let first_clip_path = clip_map.values().next()
        .ok_or_else(|| FrameForgeError::Export("no clips in map".into()))?;

    let first_ictx = ffmpeg_next::format::input(first_clip_path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

    let mut octx = match output_format {
        Some(fmt) => ffmpeg_next::format::output_as(output_path, fmt)
            .map_err(|e| FrameForgeError::Export(format!("format::output_as failed: {e}")))?,
        None => ffmpeg_next::format::output(output_path)
            .map_err(|e| FrameForgeError::Export(format!("format::output failed: {e}")))?,
    };

    let input_streams: Vec<ffmpeg_next::Stream> = first_ictx.streams().collect();
    let mut stream_mapping: Vec<i32> = vec![-1; input_streams.len()];
    let mut ist_time_bases: Vec<FfmpegRational> = vec![FfmpegRational(1, 1); input_streams.len()];

    struct VideoTranscoder {
        decoder: codec::decoder::Video,
        encoder: encoder::Video,
        ost_index: usize,
    }

    struct AudioTranscoder {
        decoder: codec::decoder::Audio,
        encoder: encoder::Audio,
        ost_index: usize,
    }

    let mut video_transcoders: Vec<Option<VideoTranscoder>> =
        (0..input_streams.len()).map(|_| None).collect();
    let mut audio_transcoders: Vec<Option<AudioTranscoder>> =
        (0..input_streams.len()).map(|_| None).collect();

    let mut ost_index = 0;
    for (ist_index, ist) in input_streams.iter().enumerate() {
        let medium = ist.parameters().medium();
        if medium != media::Type::Video && medium != media::Type::Audio {
            continue;
        }

        ist_time_bases[ist_index] = ist.time_base();
        stream_mapping[ist_index] = ost_index as i32;

        let codec_id = match medium {
            media::Type::Video => select_video_encoder_by_pref(encoder_pref),
            media::Type::Audio => select_audio_encoder_by_pref(encoder_pref),
            _ => continue,
        };

        // Create decoder.
        let dec_ctx = codec::Context::from_parameters(ist.parameters())
            .map_err(|e| FrameForgeError::Export(format!("decoder context failed: {e}")))?;
        let decoder_opened = dec_ctx
            .decoder()
            .open()
            .map_err(|e| FrameForgeError::Export(format!("decoder open failed: {e}")))?;

        // Create encoder context.
        let codec = encoder::find(codec_id).ok_or_else(|| {
            FrameForgeError::Export(format!("encoder not found for {:?}", codec_id))
        })?;
        let enc_ctx = codec::Context::new_with_codec(codec);

        if medium == media::Type::Video {
            let dec_video = decoder_opened
                .video()
                .map_err(|e| FrameForgeError::Export(format!("decoder video failed: {e}")))?;
            let mut enc_video = enc_ctx
                .encoder()
                .video()
                .map_err(|e| FrameForgeError::Export(format!("encoder video failed: {e}")))?;
            enc_video.set_width(target_width.unwrap_or(dec_video.width()));
            enc_video.set_height(target_height.unwrap_or(dec_video.height()));
            if let Some(pf) = target_pix_fmt {
                if let Some(px) = parse_pixel_format(pf) {
                    enc_video.set_format(px);
                }
            }
if let (Some(num), Some(den)) = (target_fps_num, target_fps_den) {
                enc_video.set_frame_rate(Some(ffmpeg_next::Rational(
                    num.try_into().unwrap_or(0),
                    den.try_into().unwrap_or(1),
                )));
            }
            enc_video.set_time_base(ist.time_base());

            // Apply quality preset to encoder via Dictionary options
            let mut opts = Dictionary::new();
            let preset = match quality_preset {
                "ultrafast" | "superfast" | "veryfast" | "faster" | "fast" | "medium" | "slow" | "slower" | "veryslow" => {
                    Some(quality_preset)
                }
                "high" => Some("veryslow"),
                "low" => Some("fast"),
                _ => Some("medium"),
            };
            if let Some(p) = preset {
                opts.set("preset", p);
            }

            // Set CRF for quality control (for x264/x265)
            let crf = match quality_preset {
                "high" => "18",
                "medium" => "23",
                "low" => "28",
                _ => "23",
            };
            opts.set("crf", crf);

            let opened_encoder = enc_video
                .open_with(opts)
                .map_err(|e| FrameForgeError::Export(format!("encoder open failed: {e}")))?;

            let mut ost = octx
                .add_stream(codec)
                .map_err(|e| FrameForgeError::Export(format!("add_stream failed: {e}")))?;
            ost.set_parameters(&opened_encoder);
            // SAFETY: ost.parameters() returns a valid Parameters object from the ffmpeg-next
            // output stream. The mutable pointer is safe to dereference because:
            // 1. The stream was just created by add_stream() and is guaranteed to be valid
            // 2. We only modify the codec_tag field which is a simple integer (u32)
            // 3. The Parameters object outlives this operation as it's owned by the stream
            unsafe {
                (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            }

            video_transcoders[ist_index] = Some(VideoTranscoder {
                decoder: dec_video,
                encoder: opened_encoder,
                ost_index,
            });
        } else if medium == media::Type::Audio {
            let dec_audio = decoder_opened
                .audio()
                .map_err(|e| FrameForgeError::Export(format!("decoder audio failed: {e}")))?;
            let mut enc_audio = enc_ctx
                .encoder()
                .audio()
                .map_err(|e| FrameForgeError::Export(format!("encoder audio failed: {e}")))?;
            if let Some(rate) = audio_sample_rate {
                enc_audio.set_rate(rate as i32);
            }
            if let Some(ch) = audio_channels {
                enc_audio.set_channels(ch as i32);
            }

            let opened_encoder = enc_audio
                .open_with(Dictionary::new())
                .map_err(|e| FrameForgeError::Export(format!("encoder open failed: {e}")))?;

            let mut ost = octx
                .add_stream(codec)
                .map_err(|e| FrameForgeError::Export(format!("add_stream failed: {e}")))?;
            ost.set_parameters(&opened_encoder);
            // SAFETY: ost.parameters() returns a valid Parameters object from the ffmpeg-next
            // output stream. The mutable pointer is safe to dereference because:
            // 1. The stream was just created by add_stream() and is guaranteed to be valid
            // 2. We only modify the codec_tag field which is a simple integer (u32)
            // 3. The Parameters object outlives this operation as it's owned by the stream
            unsafe {
                (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            }

            audio_transcoders[ist_index] = Some(AudioTranscoder {
                decoder: dec_audio,
                encoder: opened_encoder,
                ost_index,
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

    // Process each segment in timeline order
    for (seg_idx, segment) in plan.segments.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }

        let clip_path = clip_map.get(&segment.source_id)
            .ok_or_else(|| FrameForgeError::Export(format!("clip not found for source_id: {}", segment.source_id.0)))?;

        // Open input file for this segment's clip
        let mut ictx = ffmpeg_next::format::input(clip_path)
            .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

        // Seek to segment start using cut_points (keyframe-snapped) when available
        let seg_start_seconds = segment.cut_points.first()
            .map(|t| t.to_seconds())
            .unwrap_or_else(|| segment.range.start.to_seconds());
        let start_pts_av = (seg_start_seconds * 1_000_000.0) as i64;
        
        // Find video stream index in this file for seeking
        let has_video = ictx.streams()
            .any(|s| s.parameters().medium() == media::Type::Video);
        
        if has_video {
            ictx.seek(start_pts_av, i64::MIN..start_pts_av)
                .map_err(|e| FrameForgeError::Export(format!("seek failed: {e}")))?;
        }

        let seg_end_seconds = segment.cut_points.get(1)
            .map(|t| t.to_seconds())
            .unwrap_or_else(|| segment.range.end.to_seconds());

        // Calculate output PTS offset for segment start for each output stream.
        // This keeps the timeline continuous across segments and prevents
        // PTS values from accumulating to absurdly large numbers (e.g. 100,000 hours).
        let seg_start_output_pts: Vec<i64> = (0..ost_index as i32).map(|oi| {
            let ost = octx.stream(oi as usize)
                .expect("output stream exists");
            let out_tb_sec = ost.time_base().0 as f64 / ost.time_base().1 as f64;
            if out_tb_sec > 0.0 {
                (seg_start_seconds / out_tb_sec) as i64 + base_offsets[oi as usize]
            } else {
                base_offsets[oi as usize]
            }
        }).collect();

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
                * (ist_time_bases[ist_index].0 as f64 / ist_time_bases[ist_index].1 as f64);

            if pkt_seconds < seg_start_seconds - 1e-6 {
                continue;
            }
            // Include frames up to and including the segment end boundary so the
            // cut point is not dropped (prevents frame glitches at cut points).
            if pkt_seconds > seg_end_seconds {
                break;
            }

            let medium = stream.parameters().medium();

            if medium == media::Type::Video {
                if let Some(t) = video_transcoders[ist_index].as_mut() {
                    t.decoder.send_packet(&pkt).ok();
                    let mut frame = ffmpeg_next::util::frame::Video::empty();
                    while t.decoder.receive_frame(&mut frame).is_ok() {
                        let norm_frame =
                            normalize_video_frame(&frame, target_width, target_height, target_pix_fmt)?;
                        t.encoder.send_frame(&norm_frame).ok();
                        let mut out_pkt = packet::Packet::empty();
                        while t.encoder.receive_packet(&mut out_pkt).is_ok() {
                            out_pkt.set_stream(t.ost_index);
                            out_pkt.set_position(-1);
                            // Apply segment start offset to keep timeline continuous
                            // and prevent PTS from accumulating to absurd values.
                            let offset = seg_start_output_pts[t.ost_index as usize];
                            out_pkt.set_pts(Some(out_pkt.pts().unwrap_or(0) + offset));
                            if let Some(dts) = out_pkt.dts() {
                                out_pkt.set_dts(Some(dts + offset));
                            }
                            // Track base_offsets using final written PTS.
                            let final_pts = out_pkt.pts().unwrap_or(0);
                            base_offsets[t.ost_index as usize] = final_pts;
                            out_pkt
                                .write_interleaved(&mut octx)
                                .map_err(|e| FrameForgeError::Export(format!("write failed: {e}")))?;
                        }
                    }
                }
            } else if medium == media::Type::Audio {
                if let Some(t) = audio_transcoders[ist_index].as_mut() {
                    t.decoder.send_packet(&pkt).ok();
                    let mut frame = ffmpeg_next::util::frame::Audio::empty();
                    while t.decoder.receive_frame(&mut frame).is_ok() {
                        t.encoder.send_frame(&frame).ok();
                        let mut out_pkt = packet::Packet::empty();
                        while t.encoder.receive_packet(&mut out_pkt).is_ok() {
                            out_pkt.set_stream(t.ost_index);
                            out_pkt.set_position(-1);
                            // Apply segment start offset to keep timeline continuous
                            // and prevent PTS from accumulating to absurd values.
                            let offset = seg_start_output_pts[t.ost_index as usize];
                            out_pkt.set_pts(Some(out_pkt.pts().unwrap_or(0) + offset));
                            if let Some(dts) = out_pkt.dts() {
                                out_pkt.set_dts(Some(dts + offset));
                            }
                            // Track base_offsets using final written PTS.
                            let final_pts = out_pkt.pts().unwrap_or(0);
                            base_offsets[t.ost_index as usize] = final_pts;
                            out_pkt
                                .write_interleaved(&mut octx)
                                .map_err(|e| FrameForgeError::Export(format!("write failed: {e}")))?;
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
            // Apply accumulated base offset to keep timeline continuous.
            let offset = base_offsets[t.ost_index as usize];
            out_pkt.set_pts(Some(out_pkt.pts().unwrap_or(0) + offset));
            if let Some(dts) = out_pkt.dts() {
                out_pkt.set_dts(Some(dts + offset));
            }
            let final_pts = out_pkt.pts().unwrap_or(0);
            base_offsets[t.ost_index as usize] = final_pts;
            out_pkt
                .write_interleaved(&mut octx)
                .map_err(|e| FrameForgeError::Export(format!("write failed: {e}")))?;
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
            // Apply accumulated base offset to keep timeline continuous.
            let offset = base_offsets[t.ost_index as usize];
            out_pkt.set_pts(Some(out_pkt.pts().unwrap_or(0) + offset));
            if let Some(dts) = out_pkt.dts() {
                out_pkt.set_dts(Some(dts + offset));
            }
            let final_pts = out_pkt.pts().unwrap_or(0);
            base_offsets[t.ost_index as usize] = final_pts;
            out_pkt
                .write_interleaved(&mut octx)
                .map_err(|e| FrameForgeError::Export(format!("write failed: {e}")))?;
        }
    }

    octx.write_trailer()
        .map_err(|e| FrameForgeError::Export(format!("write_trailer failed: {e}")))?;

    progress(1.0);
    Ok(())
}

/// Select video encoder by preference name.
fn select_video_encoder_by_pref(pref: &str) -> ffmpeg_next::codec::Id {
    match pref {
        "vaapi" => ffmpeg_next::codec::Id::H264,
        "nvenc" => ffmpeg_next::codec::Id::H264,
        "x265" => ffmpeg_next::codec::Id::HEVC,
        "svtav1" => ffmpeg_next::codec::Id::AV1,
        _ => ffmpeg_next::codec::Id::H264,
    }
}

/// Select audio encoder by preference name.
fn select_audio_encoder_by_pref(pref: &str) -> ffmpeg_next::codec::Id {
    match pref {
        "aac" => ffmpeg_next::codec::Id::AAC,
        "opus" => ffmpeg_next::codec::Id::OPUS,
        "mp3" => ffmpeg_next::codec::Id::MP3,
        _ => ffmpeg_next::codec::Id::AAC,
    }
}

/// Normalize video frame: scale/pix_fmt via swscale.
fn normalize_video_frame(
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
fn parse_pixel_format(s: &str) -> Option<ffmpeg_next::format::Pixel> {
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

/// Convert ffmpeg Pixel enum to string for debugging.
fn pixel_format_to_string(p: ffmpeg_next::format::Pixel) -> &'static str {
    match p {
        ffmpeg_next::format::Pixel::YUV420P => "yuv420p",
        ffmpeg_next::format::Pixel::YUV422P => "yuv422p",
        ffmpeg_next::format::Pixel::YUV444P => "yuv444p",
        ffmpeg_next::format::Pixel::YUV410P => "yuv410p",
        ffmpeg_next::format::Pixel::YUV411P => "yuv411p",
        ffmpeg_next::format::Pixel::YUV420P9LE => "yuv420p9le",
        ffmpeg_next::format::Pixel::YUV420P10LE => "yuv420p10le",
        ffmpeg_next::format::Pixel::YUV420P12LE => "yuv420p12le",
        ffmpeg_next::format::Pixel::YUV420P14LE => "yuv420p14le",
        ffmpeg_next::format::Pixel::YUV420P16LE => "yuv420p16le",
        ffmpeg_next::format::Pixel::YUV422P9LE => "yuv422p9le",
        ffmpeg_next::format::Pixel::YUV422P10LE => "yuv422p10le",
        ffmpeg_next::format::Pixel::YUV422P12LE => "yuv422p12le",
        ffmpeg_next::format::Pixel::YUV422P14LE => "yuv422p14le",
        ffmpeg_next::format::Pixel::YUV422P16LE => "yuv422p16le",
        ffmpeg_next::format::Pixel::YUV444P9LE => "yuv444p9le",
        ffmpeg_next::format::Pixel::YUV444P10LE => "yuv444p10le",
        ffmpeg_next::format::Pixel::YUV444P12LE => "yuv444p12le",
        ffmpeg_next::format::Pixel::YUV444P14LE => "yuv444p14le",
        ffmpeg_next::format::Pixel::YUV444P16LE => "yuv444p16le",
        ffmpeg_next::format::Pixel::NV12 => "nv12",
        ffmpeg_next::format::Pixel::NV21 => "nv21",
        ffmpeg_next::format::Pixel::NV16 => "nv16",
        ffmpeg_next::format::Pixel::P010LE => "p010le",
        ffmpeg_next::format::Pixel::P016LE => "p016le",
        ffmpeg_next::format::Pixel::RGBA => "rgba",
        ffmpeg_next::format::Pixel::BGRA => "bgra",
        ffmpeg_next::format::Pixel::ARGB => "argb",
        ffmpeg_next::format::Pixel::ABGR => "abgr",
        ffmpeg_next::format::Pixel::RGB24 => "rgb24",
        ffmpeg_next::format::Pixel::BGR24 => "bgr24",
        ffmpeg_next::format::Pixel::RGB48LE => "rgb48le",
        ffmpeg_next::format::Pixel::RGB48BE => "rgb48be",
        ffmpeg_next::format::Pixel::BGR48LE => "bgr48le",
        ffmpeg_next::format::Pixel::BGR48BE => "bgr48be",
        ffmpeg_next::format::Pixel::RGBA64LE => "rgba64le",
        ffmpeg_next::format::Pixel::RGBA64BE => "rgba64be",
        ffmpeg_next::format::Pixel::BGRA64LE => "bgra64le",
        ffmpeg_next::format::Pixel::BGRA64BE => "bgra64be",
        ffmpeg_next::format::Pixel::GRAY8 => "gray8",
        ffmpeg_next::format::Pixel::GRAY9LE => "gray9le",
        ffmpeg_next::format::Pixel::GRAY10LE => "gray10le",
        ffmpeg_next::format::Pixel::GRAY12LE => "gray12le",
        ffmpeg_next::format::Pixel::GRAY14LE => "gray14le",
        ffmpeg_next::format::Pixel::GRAY16LE => "gray16le",
        ffmpeg_next::format::Pixel::GRAY16BE => "gray16be",
        ffmpeg_next::format::Pixel::YUYV422 => "yuyv422",
        ffmpeg_next::format::Pixel::UYVY422 => "uyvy422",
        ffmpeg_next::format::Pixel::YVYU422 => "vyuy422",
        ffmpeg_next::format::Pixel::YUVA420P => "yuva420p",
        ffmpeg_next::format::Pixel::YUVA422P => "yuva422p",
        ffmpeg_next::format::Pixel::YUVA444P => "yuva444p",
        ffmpeg_next::format::Pixel::GBRP => "gbrp",
        ffmpeg_next::format::Pixel::GBRP9LE => "gbrp9le",
        ffmpeg_next::format::Pixel::GBRP10LE => "gbrp10le",
        ffmpeg_next::format::Pixel::GBRP12LE => "gbrp12le",
        ffmpeg_next::format::Pixel::GBRP14LE => "gbrp14le",
        ffmpeg_next::format::Pixel::GBRP16LE => "gbrp16le",
        ffmpeg_next::format::Pixel::GBRAP => "gbrap",
        ffmpeg_next::format::Pixel::GBRAP10LE => "gbrap10le",
        ffmpeg_next::format::Pixel::GBRAP12LE => "gbrap12le",
        ffmpeg_next::format::Pixel::GBRAP16LE => "gbrap16le",
        _ => "unknown",
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

/// Determine the FFmpeg output muxer name from a file extension.
fn output_format_from_extension(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
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
    let temp = destination.with_file_name(format!(
        "{}.tmp",
        destination.file_name().unwrap().to_string_lossy()
    ));
    let fmt = output_format_from_extension(destination);
    write(&temp, fmt.or(None))?;
    std::fs::rename(&temp, destination)?;
    Ok(())
}

/// Context for remuxing: holds all per-stream state.
struct RemuxCtx<'a> {
    ictx: &'a mut ffmpeg_next::format::context::Input,
    octx: &'a mut ffmpeg_next::format::context::Output,
    stream_mapping: &'a [i32],
    ist_time_bases: &'a [FfmpegRational],
    base_offsets: &'a mut [i64],
    last_dts: &'a mut [Option<i64>],
    /// Per-stream segment start PTS (input timebase) for independent PTS rebasing.
    /// Prevents audio/video offset when the first packet after seek is not video.
    stream_seg_start: &'a mut [Option<i64>],
    video_stream_idx: Option<usize>,
}

/// Remux a single segment from input to output with timestamp rebasing.
fn remux_segment(
    ctx: &mut RemuxCtx<'_>,
    segment: &PlannedSegment,
    cancel: &CancelToken,
) -> Result<()> {
    let _source_id = segment.source_id.0 as usize;

    let seg_start_seconds = segment.cut_points.first()
        .map(|t| t.to_seconds())
        .unwrap_or_else(|| segment.range.start.to_seconds());
    let seg_end_seconds = segment.cut_points.get(1)
        .map(|t| t.to_seconds())
        .unwrap_or_else(|| segment.range.end.to_seconds());
    let start_pts_av = (seg_start_seconds * 1_000_000.0) as i64;

    if let Some(_video_idx) = ctx.video_stream_idx {
        ctx.ictx
            .seek(start_pts_av, i64::MIN..start_pts_av)
            .map_err(|e| FrameForgeError::Export(format!("seek failed: {e}")))?;
    }

    for (stream, mut packet) in ctx.ictx.packets() {
        if cancel.is_cancelled() {
            return Err(FrameForgeError::Export("export cancelled".into()));
        }

        let ist_index = stream.index() as i32;
        let ost_index = ctx.stream_mapping[ist_index as usize];
        if ost_index < 0 {
            continue;
        }

        let ist_tb = ctx.ist_time_bases[ist_index as usize];
        let tb_sec = ist_tb.0 as f64 / ist_tb.1 as f64;

        let pkt_pts = packet.pts().unwrap_or(0);
        let pkt_seconds = pkt_pts as f64 * tb_sec;

        if pkt_seconds < seg_start_seconds - 1e-6 {
            continue;
        }
        // Include frames up to and including the segment end boundary so the
        // cut point is not dropped (prevents frame glitches at cut points).
        if pkt_seconds > seg_end_seconds {
            break;
        }

        // Capture per-stream segment start PTS on the first packet of this stream.
        // This keeps audio and video independently aligned after seek.
        if ctx.stream_seg_start[ist_index as usize].is_none() {
            ctx.stream_seg_start[ist_index as usize] = Some(pkt_pts);
        }
        let stream_start_pts = ctx.stream_seg_start[ist_index as usize].unwrap_or(pkt_pts);

        // Calculate relative PTS in input timebase units (segment starts at 0).
        let relative_pts = pkt_pts - stream_start_pts;

        packet.set_pts(Some(relative_pts));

        // Handle DTS relative to stream start.
        let new_dts = packet.dts().map(|d| d - stream_start_pts);
        if let Some(d) = new_dts {
            if let Some(last) = ctx.last_dts[ost_index as usize] {
                if d <= last {
                    packet.set_dts(Some(last + 1));
                } else {
                    packet.set_dts(Some(d));
                }
            } else {
                packet.set_dts(Some(d));
            }
            ctx.last_dts[ost_index as usize] = packet.dts();
        }

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
        let adjusted_pts = pkt_pts_after + ctx.base_offsets[ost_index as usize];
        packet.set_pts(Some(adjusted_pts));

        let pkt_dts_after = packet.dts().unwrap_or(0);
        let adjusted_dts = pkt_dts_after + ctx.base_offsets[ost_index as usize];
        packet.set_dts(Some(adjusted_dts));

        if pkt_pts_after < pkt_dts_after {
            packet.set_pts(Some(pkt_dts_after));
        }

        // Update base_offsets to the last written PTS (in output timebase units).
        // Using the final PTS (not packet.duration()) handles cases where duration
        // is zero or unreliable, preventing sync drift across segments.
        let final_pts = packet.pts().unwrap_or(0);
        ctx.base_offsets[ost_index as usize] = final_pts;

        packet
            .write_interleaved(ctx.octx)
            .map_err(|e| FrameForgeError::Export(format!("write_interleaved failed: {e}")))?;
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
}
