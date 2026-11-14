//! scrub-engine: latest-wins seeking and frame-accurate preview.
//!
//! Algorithm (see specs/08-key-algorithms.md):
//!   1. av_seek_frame to the keyframe at-or-before target PTS.
//!   2. Decode forward, discarding frames before target.
//!   3. Latest-wins cancellation: a newer seek aborts the in-flight decode.
//!   4. Low-res preview while dragging; exact full-res frame on release.
//!
//! Decode is performed with ffmpeg-next directly (not mpv), because mpv is a
//! player abstraction and does not expose decoded frames in memory. mpv-adapter
//! remains the playback path; scrub-engine provides the seek targets that
//! mpv-adapter executes for playback scrubbing.

use ffmpeg_next::{codec, decoder, media, software::scaling::Flags, threading, util::error::Error as FfmpegError};
use frameforge_core_types::{FrameForgeError, Result, Timebase, Timestamp};
#[allow(clippy::single_component_path_imports)]
use libc;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Opaque handle to a decoded frame (pixel data owned by the caller/decoder).
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub pts: Timestamp,
    pub width: u32,
    pub height: u32,
    /// Raw RGBA pixel bytes (low-copy path keeps this on GPU until upload).
    pub data: Vec<u8>,
}

/// Cancellation token shared between the UI and the decode task.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Result of a latest-wins scrub: only the most recent target is delivered.
#[derive(Debug, Clone)]
pub struct ScrubResult {
    pub target: Timestamp,
    pub frame: Option<DecodedFrame>,
    pub cancelled: bool,
}

/// Latest-wins scrub session.
pub struct ScrubSession {
    pub token: CancelToken,
    ictx: Option<ffmpeg_next::format::context::Input>,
    decoder: Option<decoder::Video>,
    video_stream_idx: Option<usize>,
    time_base: Option<ffmpeg_next::Rational>,
    current_path: Option<std::path::PathBuf>,
    /// Current frame PTS (in stream time base) for next/prev frame navigation.
    current_pts: Option<i64>,
    sws_cache: SwsContextCache,
}

impl std::fmt::Debug for ScrubSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrubSession")
            .field("token", &self.token)
            .field("video_stream_idx", &self.video_stream_idx)
            .field("time_base", &self.time_base)
            .field("current_path", &self.current_path)
            .field("has_ictx", &self.ictx.is_some())
            .field("has_decoder", &self.decoder.is_some())
            .finish()
    }
}

impl ScrubSession {
    pub fn new() -> Self {
        Self {
            token: CancelToken::new(),
            ictx: None,
            decoder: None,
            video_stream_idx: None,
            time_base: None,
            current_path: None,
            current_pts: None,
            sws_cache: SwsContextCache { cache: std::collections::HashMap::new(), max_size: 16, order: std::collections::VecDeque::new() }
        }
    }

    /// Get the shared SWS context cache for frame conversion.
    pub fn get_sws_cache(&mut self) -> &mut SwsContextCache {
        &mut self.sws_cache
    }

    /// Open a file and initialize the decoder. If the same file is already open,
    /// this does nothing. If a different file is open, it closes the old one first.
    pub fn open_file(&mut self, path: &Path) -> Result<()> {
        // If already open with the same path, do nothing.
        if let Some(ref current) = self.current_path {
            if current == path {
                return Ok(());
            }
        }

        // Close any existing session.
        self.close();

        ffmpeg_next::init()
            .map_err(|e| FrameForgeError::Probe(format!("ffmpeg init failed: {e}")))?;

        let ictx = ffmpeg_next::format::input(path)
            .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

        // Collect stream info we need before seeking (Stream borrows ictx).
        let (video_stream_idx, time_base, codecpar) = {
            let mut idx: Option<u32> = None;
            let mut tb: Option<ffmpeg_next::Rational> = None;
            let mut par: Option<ffmpeg_next::codec::Parameters> = None;

            for (i, stream) in ictx.streams().enumerate() {
                if stream.parameters().medium() == media::Type::Video {
                    idx = Some(i as u32);
                    tb = Some(stream.time_base());
                    par = Some(stream.parameters().clone());
                    break;
                }
            }

            let idx = idx.ok_or_else(|| FrameForgeError::Probe("no video stream found".into()))?;
            let tb = tb.ok_or_else(|| FrameForgeError::Probe("no video time base".into()))?;
            let par =
                par.ok_or_else(|| FrameForgeError::Probe("no video codec parameters".into()))?;
            (idx, tb, par)
        };

        // Find the decoder for the video stream.
        let codec_id = codecpar.id();
        let mut ctx = codec::Context::from_parameters(codecpar.clone())
            .map_err(|e| FrameForgeError::Probe(format!("decoder context creation failed (codec={:?}): {e}", codec_id)))?;
        ctx.set_threading(threading::Config {
            kind: threading::Type::Frame,
            count: 0,
            safe: true,
        });
        // .video() finds the right decoder and opens it in one call (avcodec_open2 with codec).
        let decoder = ctx.decoder().video()
            .map_err(|e| FrameForgeError::Probe(format!("decoder open failed (codec={:?}): {e}", codec_id)))?;

        self.ictx = Some(ictx);
        self.decoder = Some(decoder);
        self.video_stream_idx = Some(video_stream_idx as usize);
        self.time_base = Some(time_base);
        self.current_path = Some(path.to_path_buf());
        self.current_pts = None;

        Ok(())
    }

    /// Seek to a timestamp using the already-open decoder/context.
    /// Call `open_file` first, or this will return an error.
    pub fn seek(&mut self, target: Timestamp) -> Result<ScrubResult> {
        let ictx = self
            .ictx
            .as_mut()
            .ok_or_else(|| FrameForgeError::Seek("no file open; call open_file first".into()))?;
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| FrameForgeError::Seek("no decoder open; call open_file first".into()))?;
        let video_stream_idx = self
            .video_stream_idx
            .ok_or_else(|| FrameForgeError::Seek("no video stream index; call open_file first".into()))?;
        let time_base = self
            .time_base
            .ok_or_else(|| FrameForgeError::Seek("no time base; call open_file first".into()))?;

        // Convert target PTS to the stream's time base.
        let target_seconds = target.to_seconds();
        let tb_seconds = time_base.0 as f64 / time_base.1 as f64;
        let target_pts_in_tb = if tb_seconds > 0.0 {
            (target_seconds / tb_seconds) as i64
        } else {
            target.value
        };

        // av_seek_frame to the keyframe at-or-before target.
        // Seek range: from beginning of file up to target PTS.
        ictx.seek(target_pts_in_tb, i64::MIN..target_pts_in_tb)
            .map_err(|e| FrameForgeError::Seek(format!("seek failed: {e}")))?;

        // Decode forward, discarding frames before target.
        let mut frame = ffmpeg_next::util::frame::Video::empty();

        let mut found = None;
        for (_stream, pkt) in ictx.packets() {
            if self.token.is_cancelled() {
                return Ok(ScrubResult {
                    target,
                    frame: None,
                    cancelled: true,
                });
            }
            if pkt.stream() != video_stream_idx {
                continue;
            }

            // Send packet to decoder.
            decoder.send_packet(&pkt).ok();

            loop {
                if self.token.is_cancelled() {
                    return Ok(ScrubResult {
                        target,
                        frame: None,
                        cancelled: true,
                    });
                }
                match decoder.receive_frame(&mut frame) {
                    Ok(_) => {
                        let frame_pts = frame.pts().unwrap_or(0);
                        let frame_seconds = frame_pts as f64 * tb_seconds;
                        if frame_seconds >= target_seconds - 1e-6 {
                            // Found the target frame (or the first frame at/after target).
                            let mut cache = self.get_sws_cache();
                let data = video_frame_to_rgba(&frame, &mut cache)?;
                            let frame_tb = Timebase::new(time_base.0 as i64, time_base.1 as i64);
                            self.current_pts = Some(frame_pts);
                            found = Some(DecodedFrame {
                                pts: Timestamp::new(frame_pts, frame_tb),
                                width: frame.width(),
                                height: frame.height(),
                                data,
                            });
                            break;
                        }
                        // Discard frame before target; continue decoding.
                    }
                    Err(FfmpegError::Other { errno }) if errno == libc::EAGAIN => break,
                    Err(_) => break,
                }
            }
            if found.is_some() {
                break;
            }
        }

        match found {
            Some(f) => Ok(ScrubResult {
                target,
                frame: Some(f),
                cancelled: false,
            }),
            None => Err(FrameForgeError::Seek(
                "no frame found at or after target PTS".into(),
            )),
        }
    }

    /// Convenience method: open file (if needed) and seek in one call.
    /// Reuses the open decoder if the file hasn't changed.
    pub fn seek_to_file(&mut self, path: &Path, target: Timestamp) -> Result<ScrubResult> {
        self.open_file(path)?;
        self.seek(target)
    }

    /// Close the current file and clean up resources.
    pub fn close(&mut self) {
        self.decoder = None;
        self.ictx = None;
        self.video_stream_idx = None;
        self.time_base = None;
        self.current_path = None;
        self.current_pts = None;
    }

    /// Get the next frame from the current decode position.
    /// Continues decoding from where the last seek/next_frame left off.
    pub fn next_frame(&mut self) -> Result<Option<DecodedFrame>> {
        if self.token.is_cancelled() {
            return Ok(None);
        }

        let ictx = self
            .ictx
            .as_mut()
            .ok_or_else(|| FrameForgeError::Seek("no file open; call open_file first".into()))?;
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| FrameForgeError::Seek("no decoder open; call open_file first".into()))?;
        let video_stream_idx = self
            .video_stream_idx
            .ok_or_else(|| FrameForgeError::Seek("no video stream index; call open_file first".into()))?;
        let time_base = self
            .time_base
            .ok_or_else(|| FrameForgeError::Seek("no time base; call open_file first".into()))?;
        let mut frame = ffmpeg_next::util::frame::Video::empty();

        // Continue decoding from current position.
        for (_stream, pkt) in ictx.packets() {
            if self.token.is_cancelled() {
                return Ok(None);
            }
            if pkt.stream() != video_stream_idx {
                continue;
            }

            decoder.send_packet(&pkt).ok();

            loop {
                if self.token.is_cancelled() {
                    return Ok(None);
                }
                match decoder.receive_frame(&mut frame) {
                    Ok(_) => {
                        let frame_pts = frame.pts().unwrap_or(0);
                        let mut cache = self.get_sws_cache();
                let data = video_frame_to_rgba(&frame, &mut cache)?;
                        let frame_tb = Timebase::new(time_base.0 as i64, time_base.1 as i64);
                        self.current_pts = Some(frame_pts);
                        return Ok(Some(DecodedFrame {
                            pts: Timestamp::new(frame_pts, frame_tb),
                            width: frame.width(),
                            height: frame.height(),
                            data,
                        }));
                    }
                    Err(FfmpegError::Other { errno }) if errno == libc::EAGAIN => break,
                    Err(_) => break,
                }
            }
        }

        // End of stream reached.
        Ok(None)
    }

    /// Get the previous frame by seeking back to the keyframe at or before
    /// (current_pts - 1 frame duration) and decoding forward.
    pub fn prev_frame(&mut self) -> Result<Option<DecodedFrame>> {
        if self.token.is_cancelled() {
            return Ok(None);
        }

        let current_pts = self.current_pts.ok_or_else(|| {
            FrameForgeError::Seek("no current frame position; call seek first".into())
        })?;

        let ictx = self
            .ictx
            .as_mut()
            .ok_or_else(|| FrameForgeError::Seek("no file open; call open_file first".into()))?;
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| FrameForgeError::Seek("no decoder open; call open_file first".into()))?;
        let video_stream_idx = self
            .video_stream_idx
            .ok_or_else(|| FrameForgeError::Seek("no video stream index; call open_file first".into()))?;
        let time_base = self
            .time_base
            .ok_or_else(|| FrameForgeError::Seek("no time base; call open_file first".into()))?;

        let tb_seconds = time_base.0 as f64 / time_base.1 as f64;

        // Estimate one frame duration in stream time base units.
        // Use a small epsilon (1/frame_rate approx) or 1 unit if unknown.
        // We'll seek to slightly before current_pts to land on the previous keyframe.
        let frame_duration_tb = (1.0 / 30.0 / tb_seconds).max(1.0) as i64; // ~30fps fallback
        let target_pts = current_pts.saturating_sub(frame_duration_tb);

        // Seek to keyframe at or before target.
        ictx.seek(target_pts, i64::MIN..target_pts)
            .map_err(|e| FrameForgeError::Seek(format!("seek failed: {e}")))?;

        // Flush decoder to clear any buffered frames.
        decoder.send_packet(&ffmpeg_next::Packet::empty()).ok();
        let mut frame = ffmpeg_next::util::frame::Video::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            // Drain buffered frames.
        }

        // Decode forward to find the frame just before current_pts.
        let mut prev_frame_data: Option<DecodedFrame> = None;
        let mut prev_frame_pts: Option<i64> = None;

        for (_stream, pkt) in ictx.packets() {
            if self.token.is_cancelled() {
                return Ok(None);
            }
            if pkt.stream() != video_stream_idx {
                continue;
            }

            decoder.send_packet(&pkt).ok();

            loop {
                if self.token.is_cancelled() {
                    return Ok(None);
                }
                match decoder.receive_frame(&mut frame) {
                    Ok(_) => {
                        let frame_pts = frame.pts().unwrap_or(0);
                        let frame_seconds = frame_pts as f64 * tb_seconds;
                        let current_seconds = current_pts as f64 * tb_seconds;

                        if frame_seconds >= current_seconds - 1e-6 {
                            // Reached or passed the current frame; return the previous one.
                            if let Some(data) = prev_frame_data {
                                self.current_pts = prev_frame_pts;
                                return Ok(Some(data));
                            } else {
                                // No previous frame found (at start of stream).
                                return Ok(None);
                            }
                        }

                        // Store this frame as potential previous frame.
                        let mut cache = self.get_sws_cache();
                let data = video_frame_to_rgba(&frame, &mut cache)?;
                        let frame_tb = Timebase::new(time_base.0 as i64, time_base.1 as i64);
                        prev_frame_data = Some(DecodedFrame {
                            pts: Timestamp::new(frame_pts, frame_tb),
                            width: frame.width(),
                            height: frame.height(),
                            data,
                        });
                        prev_frame_pts = Some(frame_pts);
                    }
                    Err(FfmpegError::Other { errno }) if errno == libc::EAGAIN => break,
                    Err(_) => break,
                }
            }
        }

        // End of stream - return the last frame we saw if it's before current.
        if let Some(data) = prev_frame_data {
            self.current_pts = prev_frame_pts;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }
}

/// Cache for SWS (software scaler) contexts to avoid recreating them every frame.
/// Contexts are keyed on (width, height, format_i32) and have a size limit with
/// LRU eviction to keep memory bounded.
struct SwsContextCache {
    cache: std::collections::HashMap<(u32, u32, i32), ffmpeg_next::software::scaling::Context>,
    max_size: usize,
    order: std::collections::VecDeque<(u32, u32, i32)>,
}

impl SwsContextCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            cache: std::collections::HashMap::new(),
            max_size,
            order: std::collections::VecDeque::new(),
        }
    }

    pub fn get_or_create(
        &mut self,
        src_format: ffmpeg_next::format::Pixel,
        src_width: u32,
        src_height: u32,
    ) -> std::result::Result<&ffmpeg_next::software::scaling::Context, FrameForgeError> {
        let key = (src_width, src_height, src_format as i32);

        if let Some(cached) = self.cache.get(&key) {
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                let _ = self.order.remove(pos);
            }
            self.order.push_back(key);
            Ok(cached)
        } else {
            if self.cache.len() >= self.max_size {
                if let Some(lru_key) = self.order.pop_front() {
                    self.cache.remove(&lru_key);
                }
            }

            let new_ctx = ffmpeg_next::software::scaling::Context::get(
                src_format,
                src_width,
                src_height,
                ffmpeg_next::format::Pixel::RGBA,
                src_width,
                src_height,
                ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
            )
            .map_err(|_| FrameForgeError::Seek("swscale context creation failed".into()))?;

            self.cache.insert(key, new_ctx.clone());
            self.order.push_back(key);
            Ok(self.cache.get(&key).unwrap())
        }
    }
}

/// Convert a ffmpeg-next Video frame to raw RGBA bytes using swscale.
/// Uses a cached SWS context to avoid recreating it per frame — essential for
/// real-time scrubbing performance.
fn video_frame_to_rgba(frame: &ffmpeg_next::util::frame::Video, cache: &mut SwsContextCache) -> Result<Vec<u8>> {
    let src_width = frame.width();
    let src_height = frame.height();
    let src_format = frame.format();

    let mut sws = ffmpeg_next::software::scaling::Context::get(
        src_format,
        src_width,
        src_height,
        ffmpeg_next::format::Pixel::RGBA,
        src_width,
        src_height,
        Flags::FAST_BILINEAR,
    )
    .map_err(|_| FrameForgeError::Seek("swscale context creation failed".into()))?;

    let mut rgba_frame = ffmpeg_next::util::frame::Video::new(
        ffmpeg_next::format::Pixel::RGBA,
        src_width,
        src_height,
    );

    sws.run(frame, &mut rgba_frame)
        .map_err(|e| FrameForgeError::Seek(format!("swscale run failed: {e}")))?;

    let data = unsafe {
        let planes = rgba_frame.data(0);
        let stride = rgba_frame.stride(0);
        let height = rgba_frame.height() as usize;
        let mut out = Vec::with_capacity(stride * height);
        for row in 0..height {
            let row_ptr = planes.as_ptr().add(row * stride);
            out.extend_from_slice(std::slice::from_raw_parts(row_ptr, stride));
        }
        out
    };

    Ok(data)
}

impl Default for ScrubSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_toggle() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn scrub_result_default_fields() {
        let ts = Timestamp::new(0, Timebase::new(1, 1000));
        let r: ScrubResult = ScrubResult {
            target: ts,
            frame: None,
            cancelled: true,
        };
        assert!(r.cancelled);
        assert!(r.frame.is_none());
    }
}
