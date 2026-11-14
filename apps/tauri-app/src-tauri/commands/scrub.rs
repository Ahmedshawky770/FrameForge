use crate::state::{LatencyInfo, ScrubFrameResult, ScrubState};
use base64::engine::general_purpose;
use base64::Engine as _;
use frameforge_core_types::{Timebase, Timestamp};
use frameforge_scrub_engine::ScrubSession;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;
use tauri::{command, State};

#[command]
pub async fn player_scrub_seek(
    scrub: State<'_, Mutex<ScrubState>>,
    path: String,
    seconds: f64,
) -> Result<ScrubFrameResult, String> {
    let start = Instant::now();

    let mut s = scrub.lock().map_err(|e| e.to_string())?;
    let tb = Timebase::new(1, 1_000_000);
    let ts = Timestamp::new((seconds * 1_000_000.0) as i64, tb);

    // Get or create the ScrubSession, reusing it across seeks.
    let session = s.session.get_or_insert_with(ScrubSession::new);

    let result = session
        .seek_to_file(Path::new(&path), ts)
        .map_err(|e| e.to_string())?;

    let elapsed = start.elapsed().as_millis() as u64;
    s.latency.record(elapsed);

    let data_url = if let Some(ref frame) = result.frame {
        let mime = "image/rgba";
        let encoded = general_purpose::STANDARD.encode(&frame.data);
        format!("data:{mime};base64,{encoded}")
    } else {
        String::new()
    };

    let frame_result = ScrubFrameResult {
        data_url,
        width: result.frame.as_ref().map(|f| f.width).unwrap_or(0),
        height: result.frame.as_ref().map(|f| f.height).unwrap_or(0),
        pts_seconds: result
            .frame
            .as_ref()
            .map(|f| f.pts.to_seconds())
            .unwrap_or(0.0),
        latency_ms: elapsed,
    };

    s.last_frame = Some(frame_result.clone());
    Ok(frame_result)
}

#[command]
pub async fn player_scrub_next_frame(
    scrub: State<'_, Mutex<ScrubState>>,
) -> Result<ScrubFrameResult, String> {
    let start = Instant::now();

    let mut s = scrub.lock().map_err(|e| e.to_string())?;
    let session = s.session.as_mut().ok_or("no active scrub session")?;

    let result = session.next_frame().map_err(|e| e.to_string())?;

    let elapsed = start.elapsed().as_millis() as u64;
    s.latency.record(elapsed);

    let frame_result = if let Some(frame) = result {
        let data_url = {
            let mime = "image/rgba";
            let encoded = general_purpose::STANDARD.encode(&frame.data);
            format!("data:{mime};base64,{encoded}")
        };
        ScrubFrameResult {
            data_url,
            width: frame.width,
            height: frame.height,
            pts_seconds: frame.pts.to_seconds(),
            latency_ms: elapsed,
        }
    } else {
        // End of stream - return empty result
        ScrubFrameResult {
            data_url: String::new(),
            width: 0,
            height: 0,
            pts_seconds: 0.0,
            latency_ms: elapsed,
        }
    };

    s.last_frame = Some(frame_result.clone());
    Ok(frame_result)
}

#[command]
pub async fn player_scrub_prev_frame(
    scrub: State<'_, Mutex<ScrubState>>,
) -> Result<ScrubFrameResult, String> {
    let start = Instant::now();

    let mut s = scrub.lock().map_err(|e| e.to_string())?;
    let session = s.session.as_mut().ok_or("no active scrub session")?;

    let result = session.prev_frame().map_err(|e| e.to_string())?;

    let elapsed = start.elapsed().as_millis() as u64;
    s.latency.record(elapsed);

    let frame_result = if let Some(frame) = result {
        let data_url = {
            let mime = "image/rgba";
            let encoded = general_purpose::STANDARD.encode(&frame.data);
            format!("data:{mime};base64,{encoded}")
        };
        ScrubFrameResult {
            data_url,
            width: frame.width,
            height: frame.height,
            pts_seconds: frame.pts.to_seconds(),
            latency_ms: elapsed,
        }
    } else {
        // Beginning of stream or no previous frame - return empty result
        ScrubFrameResult {
            data_url: String::new(),
            width: 0,
            height: 0,
            pts_seconds: 0.0,
            latency_ms: elapsed,
        }
    };

    s.last_frame = Some(frame_result.clone());
    Ok(frame_result)
}

#[command]
pub async fn scrub_latency(handle: State<'_, Mutex<ScrubState>>) -> Result<LatencyInfo, String> {
    let s = handle.lock().map_err(|e| e.to_string())?;
    Ok(LatencyInfo {
        p50_ms: s.latency.p50().unwrap_or(0),
        p95_ms: s.latency.p95().unwrap_or(0),
        count: s.latency.samples.len() as u32,
    })
}