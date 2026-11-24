//! VAAPI hardware-frame upload for hardware H.264/HEVC encoders.
//!
//! `h264_vaapi` / `hevc_vaapi` only accept `AV_PIX_FMT_VAAPI` frames, so every
//! software frame produced by the decode/normalize pipeline is uploaded into a
//! VAAPI surface before it is handed to the encoder.
//!
//! Every fallible step returns `Err` instead of panicking, so the caller can
//! fall back to software encoding when no usable device exists or the driver
//! rejects the frames context. `FRAMEFORGE_DISABLE_HW=1` disables the whole
//! path for troubleshooting.

use ffmpeg_next::codec;
use ffmpeg_next::ffi::*;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::util::frame;

/// `""` lets libav pick its default device; the explicit nodes cover the common
/// Linux render-node layouts.
pub(crate) const DEVICE_CANDIDATES: &[&str] = &[
    "",
    "/dev/dri/renderD128",
    "/dev/dri/renderD129",
    "/dev/dri/card0",
    "/dev/dri/card1",
];

/// VAAPI device + frames context used for uploads.
///
/// [`VaapiFrames::attach`] transfers the device and frames references it
/// creates to the codec context (libavcodec owns and frees them). This struct
/// keeps its own reference to the frames context so per-frame buffers can be
/// allocated for the lifetime of the encoder.
pub struct VaapiFrames {
    frames: *mut AVBufferRef,
}

// The raw pointer is owned by this struct and only touched from the export
// thread that drives the encoder.
unsafe impl Send for VaapiFrames {}

impl Drop for VaapiFrames {
    fn drop(&mut self) {
        unsafe {
            if !self.frames.is_null() {
                av_buffer_unref(&mut self.frames);
            }
        }
    }
}

impl VaapiFrames {
    /// Create a VAAPI device + frames context and attach them to `enc_ctx`.
    ///
    /// `sw_format` must match the pixel format of the software frames that will
    /// be uploaded (i.e. the output of the normalize step).
    ///
    /// # Safety
    /// `enc_ctx` must not be opened before this call and must outlive the
    /// returned value.
    pub unsafe fn attach(
        enc_ctx: &mut codec::Context,
        width: u32,
        height: u32,
        sw_format: Pixel,
    ) -> Result<Self, String> {
        let mut device = create_device()?;

        let mut frames = av_hwframe_ctx_alloc(device);
        if frames.is_null() {
            av_buffer_unref(&mut device);
            return Err("av_hwframe_ctx_alloc failed".into());
        }

        {
            let hwctx = (*frames).data as *mut AVHWFramesContext;
            if hwctx.is_null() {
                av_buffer_unref(&mut frames);
                av_buffer_unref(&mut device);
                return Err("AVHWFramesContext is null".into());
            }
            (*hwctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*hwctx).sw_format = sw_format.into();
            (*hwctx).width = width as libc::c_int;
            (*hwctx).height = height as libc::c_int;
        }

        let ret = av_hwframe_ctx_init(frames);
        if ret != 0 {
            av_buffer_unref(&mut frames);
            av_buffer_unref(&mut device);
            return Err(format!(
                "av_hwframe_ctx_init failed (code {ret}) for {}x{} {:?}",
                width, height, sw_format
            ));
        }

        // Keep a private reference so we can allocate frames after ownership of
        // the original reference has been handed to the codec context.
        let mut frames_keep = av_buffer_ref(frames);
        if frames_keep.is_null() {
            av_buffer_unref(&mut frames);
            av_buffer_unref(&mut device);
            return Err("av_buffer_ref(frames) failed".into());
        }

        let ctx = enc_ctx.as_mut_ptr();
        if ctx.is_null() {
            av_buffer_unref(&mut frames_keep);
            av_buffer_unref(&mut frames);
            av_buffer_unref(&mut device);
            return Err("codec context is null".into());
        }

        // Ownership moves to libavcodec here: it frees both references (see the
        // AVCodecContext::hw_device_ctx / hw_frames_ctx documentation).
        (*ctx).hw_device_ctx = device;
        (*ctx).hw_frames_ctx = frames;

        Ok(Self {
            frames: frames_keep,
        })
    }

    /// Upload `sw` into a fresh VAAPI surface and hand it to `encoder`.
    ///
    /// # Safety
    /// The encoder must have been opened with `Pixel::VAAPI` and the frames
    /// context attached by [`VaapiFrames::attach`].
    pub unsafe fn upload(
        &self,
        encoder: &mut codec::encoder::Video,
        sw: &frame::Video,
    ) -> Result<(), String> {
        let mut hw = av_frame_alloc();
        if hw.is_null() {
            return Err("av_frame_alloc failed".into());
        }

        (*hw).format = AVPixelFormat::AV_PIX_FMT_VAAPI as libc::c_int;

        let ret = av_hwframe_get_buffer(self.frames, hw, 0);
        if ret != 0 {
            av_frame_free(&mut hw);
            return Err(format!("av_hwframe_get_buffer failed (code {ret})"));
        }

        let ret = av_hwframe_transfer_data(hw, sw.as_ptr(), 0);
        if ret != 0 {
            av_frame_free(&mut hw);
            return Err(format!("VAAPI upload failed (code {ret})"));
        }

        (*hw).pts = sw.pts().unwrap_or(0);

        let video = frame::video::Video::wrap(hw);
        let sent = encoder.send_frame(&video);
        // `send_frame` takes its own reference, so the non-owning wrapper can
        // be dropped and the frame freed below.
        drop(video);
        av_frame_free(&mut hw);

        sent.map_err(|e| format!("encoder send_frame failed: {e}"))
    }
}

/// Cheap availability probe: can any VAAPI device be created?
///
/// Used to decide up front whether a `vaapi` preference can be honoured, so the
/// common "no VAAPI hardware" case falls back to software without attempting a
/// full encoder setup.
pub fn device_available() -> bool {
    // SAFETY: create_device returns an owned AVBufferRef or an error; the
    // reference is released immediately.
    unsafe {
        match create_device() {
            Ok(mut dev) => {
                av_buffer_unref(&mut dev);
                true
            }
            Err(_) => false,
        }
    }
}

/// Try each candidate VAAPI device until one initialises.
unsafe fn create_device() -> Result<*mut AVBufferRef, String> {
    let mut last = String::from("no VAAPI device candidate available");
    for cand in DEVICE_CANDIDATES {
        let c = match std::ffi::CString::new(*cand) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let ptr = if cand.is_empty() {
            std::ptr::null()
        } else {
            c.as_ptr()
        };

        let mut dev: *mut AVBufferRef = std::ptr::null_mut();
        let ret = av_hwdevice_ctx_create(
            &mut dev,
            AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            ptr,
            std::ptr::null_mut(),
            0,
        );
        if ret == 0 && !dev.is_null() {
            return Ok(dev);
        }

        last = format!("VAAPI device '{}' rejected (code {ret})", cand);
        if !dev.is_null() {
            av_buffer_unref(&mut dev);
        }
    }

    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_include_default_and_render_nodes() {
        // Guards against an empty candidate list, which would make every
        // VAAPI export fall back to software without a useful reason.
        assert!(!DEVICE_CANDIDATES.is_empty());
        assert!(DEVICE_CANDIDATES.contains(&""));
        assert!(DEVICE_CANDIDATES.contains(&"/dev/dri/renderD128"));
    }

    #[test]
    fn vaapi_pixel_format_round_trips() {
        // `Pixel::VAAPI` is cfg-gated across ffmpeg versions, so go through
        // the libav constant and the `Pixel` conversion instead.
        let fmt = AVPixelFormat::AV_PIX_FMT_VAAPI;
        let pixel: Pixel = fmt.into();
        let back: AVPixelFormat = pixel.into();
        assert_eq!(back as libc::c_int, fmt as libc::c_int);
    }
}
