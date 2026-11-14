//! media-probe: probe input files and build keyframe indexes using ffmpeg-next.

use ffmpeg_next::Rational;
use frameforge_core_types::{FrameForgeError, Timebase, Timestamp};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: u32,
    pub kind: StreamKind,
    pub codec: String,
    pub codec_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    pub timebase_num: i64,
    pub timebase_den: i64,
    pub pix_fmt: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatInfo {
    pub format_name: String,
    pub duration_sec: Option<f64>,
    pub bit_rate: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub streams: Vec<StreamInfo>,
    pub format: FormatInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyframeEntry {
    pub pts: Timestamp,
    pub file_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct KeyframeIndex {
    pub entries: Vec<KeyframeEntry>,
}

impl KeyframeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Latest keyframe with pts <= target, or None.
    pub fn latest_keyframe_at_or_before(&self, target: &Timestamp) -> Option<&KeyframeEntry> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.pts.to_seconds() <= target.to_seconds())
    }
}

fn stream_kind(codec_type: ffmpeg_next::media::Type) -> StreamKind {
    match codec_type {
        ffmpeg_next::media::Type::Video => StreamKind::Video,
        ffmpeg_next::media::Type::Audio => StreamKind::Audio,
        ffmpeg_next::media::Type::Subtitle => StreamKind::Subtitle,
        _ => StreamKind::Other,
    }
}

fn codec_name(codecpar: &ffmpeg_next::codec::Parameters) -> String {
    codecpar.id().name().to_string()
}

fn probe_stream(stream: &ffmpeg_next::Stream) -> StreamInfo {
    let codecpar = stream.parameters();
    let time_base = stream.time_base();
    let fps = stream.avg_frame_rate();

    // Read width/height/format/sample_rate/channels via unsafe FFI
    // SAFETY: codecpar is a reference to ffmpeg_next::codec::Parameters obtained from
    // stream.parameters(), which borrows the InputContext (ictx). The InputContext is
    // alive for the duration of this function (it's created at the start of probe() and
    // not dropped until the end). The pointer returned by as_ptr() is guaranteed to be
    // valid (non-null) by ffmpeg-next's API contract for a valid Parameters object.
    // We check for null defensively. The dereference only reads simple integer fields
    // (width, height, format, sample_rate, channels) which are valid for the lifetime
    // of the Parameters object.
    let (width, height, pix_fmt, sample_rate, channels) = unsafe {
        let params_ptr = codecpar.as_ptr();
        if params_ptr.is_null() {
            (None, None, None, None, None)
        } else {
            let p = &*params_ptr;
            let w = if p.width != 0 {
                Some(p.width as u32)
            } else {
                None
            };
            let h = if p.height != 0 {
                Some(p.height as u32)
            } else {
                None
            };
            let fmt = if p.format != 0 {
                Some(format!("{:?}", p.format))
            } else {
                None
            };
            let sr = if p.sample_rate != 0 {
                Some(p.sample_rate as u32)
            } else {
                None
            };
            let ch = if p.channels != 0 {
                Some(p.channels as u32)
            } else {
                None
            };
            (w, h, fmt, sr, ch)
        }
    };

    StreamInfo {
        index: stream.index() as u32,
        kind: stream_kind(codecpar.medium()),
        codec: codec_name(&codecpar),
        codec_type: format!("{:?}", codecpar.medium()),
        width,
        height,
        fps_num: if fps.numerator() != 0 {
            Some(fps.numerator() as u32)
        } else {
            None
        },
        fps_den: if fps.denominator() != 0 {
            Some(fps.denominator() as u32)
        } else {
            None
        },
        timebase_num: time_base.0 as i64,
        timebase_den: time_base.1 as i64,
        pix_fmt,
        sample_rate,
        channels,
        profile: None,
    }
}

/// Probe a media file and return stream + format info.
pub fn probe(path: &Path) -> Result<ProbeResult, FrameForgeError> {
    ffmpeg_next::init().map_err(|e| FrameForgeError::Probe(format!("ffmpeg init failed: {e}")))?;

    let ictx = ffmpeg_next::format::input(path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

    let mut streams = Vec::new();
    for stream in ictx.streams() {
        streams.push(probe_stream(&stream));
    }

    let duration_sec = unsafe {
        let p = ictx.as_ptr();
        if p.is_null() {
            None
        } else {
            let d = (*p).duration;
            if d == ffmpeg_next::ffi::AV_NOPTS_VALUE {
                None
            } else {
                Some(d as f64 / 1_000_000.0)
            }
        }
    };
    let bit_rate = unsafe {
        let p = ictx.as_ptr();
        if p.is_null() {
            None
        } else {
            let br = (*p).bit_rate;
            if br == 0 {
                None
            } else {
                Some(br as u64)
            }
        }
    };

    Ok(ProbeResult {
        streams,
        format: FormatInfo {
            format_name: ictx.format().name().to_string(),
            duration_sec,
            bit_rate,
        },
    })
}

/// Build a keyframe index by reading packets sequentially.
pub fn build_keyframe_index(path: &Path) -> Result<KeyframeIndex, FrameForgeError> {
    ffmpeg_next::init().map_err(|e| FrameForgeError::Probe(format!("ffmpeg init failed: {e}")))?;

    let mut ictx = ffmpeg_next::format::input(path)
        .map_err(|e| FrameForgeError::Probe(format!("format::input failed: {e}")))?;

    // First pass: find video stream index and its time base
    let mut video_stream_idx: Option<usize> = None;
    let mut time_base: Rational = Rational(1, 1000);
    for stream in ictx.streams() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Video {
            video_stream_idx = Some(stream.index());
            time_base = stream.time_base();
            break;
        }
    }

    let video_stream_idx =
        video_stream_idx.ok_or_else(|| FrameForgeError::Probe("no video stream found".into()))?;

    let tb_num = time_base.0 as i64;
    let tb_den = time_base.1 as i64;

    // Read packets and collect keyframes for the video stream
    let mut entries = Vec::new();
    for (stream, packet) in ictx.packets() {
        if stream.index() != video_stream_idx {
            continue;
        }
        if packet.is_key() {
            let pts = packet.pts().unwrap_or_else(|| packet.dts().unwrap_or(0));
            entries.push(KeyframeEntry {
                pts: Timestamp::new(pts, Timebase::new(tb_num, tb_den)),
                file_offset: packet.position() as u64,
            });
        }
    }

    Ok(KeyframeIndex { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_index_empty() {
        let idx = KeyframeIndex::new();
        assert!(idx.entries.is_empty());
    }

    #[test]
    #[ignore = "requires test-media/test-video.mp4; run manually with --test media-probe -- --ignored"]
    fn probe_real_file() {
        let path = Path::new("test-media/test-video.mp4");
        if !path.exists() {
            return;
        }
        let result = probe(path).expect("probe should succeed on real file");
        assert!(
            !result.streams.is_empty(),
            "should have at least one stream"
        );
        assert!(!result.format.format_name.is_empty());
        let video = result.streams.iter().find(|s| s.kind == StreamKind::Video);
        assert!(video.is_some(), "should have a video stream");
        let v = video.unwrap();
        assert!(v.width.is_some() && v.height.is_some());
    }

    #[test]
    #[ignore = "requires test-media/test-video.mp4; run manually with --test media-probe -- --ignored"]
    fn build_keyframe_index_real_file() {
        let path = Path::new("test-media/test-video.mp4");
        if !path.exists() {
            return;
        }
        let idx = build_keyframe_index(path).expect("index build should succeed");
        // Keyframe index may be empty for some files; just ensure it doesn't crash.
        let _ = idx.entries.len();
    }
}
