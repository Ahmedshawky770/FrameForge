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

use ffmpeg_next::{codec, decoder, media, threading, util::error::Error as FfmpegError};
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
    pub current_path: Option<std::path::PathBuf>,
    /// Current frame PTS (in stream time base) for next/prev frame navigation.
    pub current_pts: Option<i64>,
    sws_cache: SwsCache,
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
            sws_cache: SwsCache::new(),
        }
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

            for stream in ictx.streams() {
                if stream.parameters().medium() == media::Type::Video {
                    idx = Some(stream.index() as u32);
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
        let mut ctx = codec::Context::from_parameters(codecpar.clone()).map_err(|e| {
            FrameForgeError::Probe(format!(
                "decoder context creation failed (codec={:?}): {e}",
                codec_id
            ))
        })?;
        ctx.set_threading(threading::Config {
            kind: threading::Type::Frame,
            count: 0,
            safe: true,
        });
        // .video() finds the right decoder and opens it in one call (avcodec_open2 with codec).
        let decoder = ctx.decoder().video().map_err(|e| {
            FrameForgeError::Probe(format!("decoder open failed (codec={:?}): {e}", codec_id))
        })?;

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
        let Self {
            token,
            ictx,
            decoder,
            video_stream_idx,
            time_base,
            sws_cache,
            current_pts,
            ..
        } = self;

        let video_stream_idx = video_stream_idx.ok_or_else(|| {
            FrameForgeError::Seek("no video stream index; call open_file first".into())
        })?;

        let time_base = time_base
            .ok_or_else(|| FrameForgeError::Seek("no time base; call open_file first".into()))?;

        let ictx = ictx
            .as_mut()
            .ok_or_else(|| FrameForgeError::Seek("no file open; call open_file first".into()))?;
        let decoder = decoder
            .as_mut()
            .ok_or_else(|| FrameForgeError::Seek("no decoder open; call open_file first".into()))?;

        // Seek uses AV_TIME_BASE (microseconds), so convert target seconds.
        let target_seconds = target.to_seconds();
        let target_us = (target_seconds * 1_000_000.0) as i64;

        // av_seek_frame to the keyframe at-or-before target.
        // Seek range: from beginning of file up to target PTS.
        ictx.seek(target_us, i64::MIN..target_us)
            .map_err(|e| FrameForgeError::Seek(format!("seek failed: {e}")))?;

        // Flush decoder buffers after seek (proper API, not EOF-inducing empty packet).
        decoder.flush();

        // Decode forward, discarding frames before target.
        let mut frame = ffmpeg_next::util::frame::Video::empty();

        let mut found = None;
        let mut _dbg_total = 0usize;
        for (_stream, pkt) in ictx.packets() {
            if token.is_cancelled() {
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
                if token.is_cancelled() {
                    return Ok(ScrubResult {
                        target,
                        frame: None,
                        cancelled: true,
                    });
                }
                match decoder.receive_frame(&mut frame) {
                    Ok(_) => {
                        let frame_pts = frame.pts().unwrap_or(0);
                        let tb_seconds = time_base.0 as f64 / time_base.1 as f64;
                        let frame_seconds = frame_pts as f64 * tb_seconds;
                        if frame_seconds >= target_seconds - 1e-6 {
                            // Found the target frame (or the first frame at/after target).
                            let out_w = frame.width();
                            let out_h = frame.height();
                            let data = video_frame_to_rgba(&frame, sws_cache, out_w, out_h)?;
                            *current_pts = Some(frame_pts);
                            found = Some(DecodedFrame {
                                pts: Timestamp::new(
                                    frame_pts,
                                    Timebase::new(time_base.0 as i64, time_base.1 as i64),
                                ),
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
        let video_stream_idx = self.video_stream_idx.ok_or_else(|| {
            FrameForgeError::Seek("no video stream index; call open_file first".into())
        })?;
        let time_base = self
            .time_base
            .ok_or_else(|| FrameForgeError::Seek("no time base; call open_file first".into()))?;

        let _tb_seconds = time_base.0 as f64 / time_base.1 as f64;
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

            if self.token.is_cancelled() {
                return Ok(None);
            }
            match decoder.receive_frame(&mut frame) {
                Ok(_) => {
                    let frame_pts = frame.pts().unwrap_or(0);
                    let out_w = frame.width();
                    let out_h = frame.height();
                    let data = video_frame_to_rgba(&frame, &mut self.sws_cache, out_w, out_h)?;
                    let frame_tb = Timebase::new(time_base.0 as i64, time_base.1 as i64);
                    self.current_pts = Some(frame_pts);
                    return Ok(Some(DecodedFrame {
                        pts: Timestamp::new(frame_pts, frame_tb),
                        width: frame.width(),
                        height: frame.height(),
                        data,
                    }));
                }
                Err(FfmpegError::Other { errno }) if errno == libc::EAGAIN => {}
                Err(_) => {}
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
        let video_stream_idx = self.video_stream_idx.ok_or_else(|| {
            FrameForgeError::Seek("no video stream index; call open_file first".into())
        })?;
        let time_base = self
            .time_base
            .ok_or_else(|| FrameForgeError::Seek("no time base; call open_file first".into()))?;

        let tb_seconds = time_base.0 as f64 / time_base.1 as f64;

        // Estimate one frame duration in stream time base units.
        // Use a small epsilon (1/frame_rate approx) or 1 unit if unknown.
        // We'll seek to slightly before current_pts to land on the previous keyframe.
        let frame_duration_tb = (1.0 / 30.0 / tb_seconds).max(1.0) as i64; // ~30fps fallback
        let target_pts = current_pts.saturating_sub(frame_duration_tb);

        // Convert stream time-base units to microseconds for seek (AV_TIME_BASE).
        let target_us = (target_pts as f64 * tb_seconds * 1_000_000.0) as i64;

        // Seek to keyframe at or before target.
        ictx.seek(target_us, i64::MIN..target_us)
            .map_err(|e| FrameForgeError::Seek(format!("seek failed: {e}")))?;

        // Flush decoder buffers after seek.
        decoder.flush();

        // Decode forward to find the frame just before current_pts.
        let mut frame = ffmpeg_next::util::frame::Video::empty();
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
                        let tb_seconds = time_base.0 as f64 / time_base.1 as f64;
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
                        let out_w = frame.width();
                        let out_h = frame.height();
                        let data = video_frame_to_rgba(&frame, &mut self.sws_cache, out_w, out_h)?;
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

/// A thread-safe handle to an FFmpeg `SwsContext` software scaler.
///
/// `ffmpeg_next::software::scaling::Context` wraps `*mut SwsContext`, a raw
/// pointer, so it is neither `Send` nor `Sync` by default. This newtype
/// manually implements both markers. That is sound because:
///
///   * Ownership is never shared between threads: `Send` is safe since the
///     pointer is just an address and FFmpeg does not bind a scaler to the
///     thread that created it.
///   * All mutation goes through `&mut self` (e.g. `run`), which Rust enforces,
///     and the tauri layer serializes every access behind `Mutex<ScrubState>`,
///     so `&Self` can never observe unsynchronized mutation (`Sync`).
struct SwsContextHandle(ffmpeg_next::software::scaling::Context);

// SAFETY: See the safety comment on `SwsContextHandle`.
unsafe impl Send for SwsContextHandle {}
unsafe impl Sync for SwsContextHandle {}

impl SwsContextHandle {
    pub fn get(
        src_format: ffmpeg_next::format::Pixel,
        src_w: u32,
        src_h: u32,
        dst_format: ffmpeg_next::format::Pixel,
        dst_w: u32,
        dst_h: u32,
        flags: ffmpeg_next::software::scaling::Flags,
    ) -> std::result::Result<Self, ffmpeg_next::Error> {
        ffmpeg_next::software::scaling::Context::get(
            src_format, src_w, src_h, dst_format, dst_w, dst_h, flags,
        )
        .map(Self)
    }

    pub fn run(
        &mut self,
        input: &ffmpeg_next::frame::Video,
        output: &mut ffmpeg_next::frame::Video,
    ) -> std::result::Result<(), ffmpeg_next::Error> {
        self.0.run(input, output)
    }
}

/// Cache for a single SWS (software scaler) context to avoid recreating it per frame.
/// The context is keyed on (src_w, src_h, src_fmt_i32, out_w, out_h) and reused across frames.
struct SwsCache {
    ctx: Option<SwsContextHandle>,
    key: Option<(u32, u32, i32, u32, u32)>, // src w,h,fmt + dst w,h
}

impl SwsCache {
    pub fn new() -> Self {
        Self {
            ctx: None,
            key: None,
        }
    }
}

/// Convert a ffmpeg-next Video frame to raw RGBA bytes using swscale.
/// Uses a cached SWS context to avoid recreating it per frame — essential for
/// real-time scrubbing performance.
fn video_frame_to_rgba(
    frame: &ffmpeg_next::util::frame::Video,
    cache: &mut SwsCache,
    out_w: u32,
    out_h: u32,
) -> std::result::Result<Vec<u8>, FrameForgeError> {
    let src_width = frame.width();
    let src_height = frame.height();
    let src_pixel = frame.format();
    let src_format = src_pixel as i32;
    let key = (src_width, src_height, src_format, out_w, out_h);

    if cache.ctx.is_none() || cache.key != Some(key) {
        cache.ctx = Some(
            SwsContextHandle::get(
                src_pixel,
                src_width,
                src_height,
                ffmpeg_next::format::Pixel::RGBA,
                out_w,
                out_h,
                ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
            )
            .map_err(|e| FrameForgeError::Probe(format!("sws create failed: {e}")))?,
        );
        cache.key = Some(key);
    }
    let scaler = cache.ctx.as_mut().unwrap();
    let mut rgba = ffmpeg_next::frame::Video::empty();
    scaler
        .run(frame, &mut rgba)
        .map_err(|e| FrameForgeError::Probe(format!("sws run failed: {e}")))?;
    // نسخ صف صف (الـ stride فيه padding)
    let stride = rgba.stride(0);
    let row = out_w as usize * 4;
    let data = rgba.data(0);
    let mut out = Vec::with_capacity(row * out_h as usize);
    for y in 0..out_h as usize {
        out.extend_from_slice(&data[y * stride..y * stride + row]);
    }
    Ok(out)
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
