use std::sync::Mutex;

use frameforge_export_engine::CancelToken;
use serde::{Deserialize, Serialize};

/// Window XID state for mpv video embedding
#[derive(Debug, Clone, Default)]
pub struct WindowXidState {
    /// XID of the Tauri WebView main window (mpv `wid` fallback).
    xid: Option<u64>,
    /// XID of a dedicated X11 child window sized to #video-surface.
    /// mpv renders into this child window so it cannot cover the whole app.
    child_xid: Option<u64>,
}

impl WindowXidState {
    pub fn set_xid(&mut self, xid: u64) {
        self.xid = Some(xid);
    }
    pub fn get_xid(&self) -> Option<u64> {
        self.xid
    }
    pub fn set_child_xid(&mut self, xid: u64) {
        self.child_xid = Some(xid);
    }
    pub fn get_child_xid(&self) -> Option<u64> {
        self.child_xid
    }
}

/// Wayland detection state
#[derive(Debug, Clone, Default)]
pub struct WaylandState {
    is_wayland: bool,
}

impl WaylandState {
    pub fn set_wayland(&mut self, is_wayland: bool) {
        self.is_wayland = is_wayland;
    }
    pub fn is_wayland(&self) -> bool {
        self.is_wayland
    }
}

/// ---- i18n / locale ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocaleInfo {
    pub current: String,
    pub available: Vec<String>,
    pub direction: String, // "ltr" | "rtl"
}

pub const AVAILABLE_LOCALES: &[&str] = &["en", "ar"];
pub const LOCALE_DIRECTIONS: &[(&str, &str)] = &[("en", "ltr"), ("ar", "rtl")];

pub fn locale_direction(locale: &str) -> &str {
    LOCALE_DIRECTIONS
        .iter()
        .find(|(l, _)| *l == locale)
        .map(|(_, d)| *d)
        .unwrap_or("ltr")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocaleState {
    pub current: String,
}

impl Default for LocaleState {
    fn default() -> Self {
        Self {
            current: "en".to_string(),
        }
    }
}

/// ---- Theme ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeInfo {
    pub current: String,   // "light" | "dark" | "system"
    pub effective: String, // "light" | "dark"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeState {
    pub current: String,
}

impl Default for ThemeState {
    fn default() -> Self {
        Self {
            current: "system".to_string(),
        }
    }
}

/// ---- Scrubbing (precision frame-accurate preview) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrubFrameResult {
    pub data_url: String, // data:image/rgba;base64,...
    pub width: u32,
    pub height: u32,
    pub pts_seconds: f64,
    pub latency_ms: u64,
}

/// Latency samples for p50/p95 measurement.
#[derive(Debug, Clone, Default)]
pub struct LatencySamples {
    pub samples: Vec<u64>,
}

impl LatencySamples {
    pub fn record(&mut self, ms: u64) {
        self.samples.push(ms);
        // Keep only last 1000 samples to bound memory.
        if self.samples.len() > 1000 {
            self.samples.drain(..=self.samples.len() - 1001);
        }
    }

    pub fn p50(&self) -> Option<u64> {
        percentile(&self.samples, 50)
    }

    pub fn p95(&self) -> Option<u64> {
        percentile(&self.samples, 95)
    }
}

pub fn percentile(samples: &[u64], p: u64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (p as f64 / 100.0) * (sorted.len() as f64 - 1.0);
    let idx = rank as usize;
    let frac = rank - idx as f64;
    if idx + 1 < sorted.len() {
        Some(sorted[idx] + ((sorted[idx + 1] - sorted[idx]) as f64 * frac) as u64)
    } else {
        Some(sorted[idx])
    }
}

#[derive(Debug, Default)]
pub struct ScrubState {
    pub latency: LatencySamples,
    pub last_frame: Option<ScrubFrameResult>,
    pub session: Option<frameforge_scrub_engine::ScrubSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyInfo {
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub count: u32,
}

/// ---- Subtitle management ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfoResponse {
    pub index: u32,
    pub kind: String,
    pub title: Option<String>,
    pub lang: Option<String>,
    pub selected: bool,
}

/// ---- Project management ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityInfo {
    pub lossless_possible: bool,
    pub reasons: Vec<String>,
    pub alternative: String,
}

/// ---- Export progress / cancellation ----
///
/// Shared between the `project_export` command (runs ffmpeg) and the
/// `project_export_cancel` / `project_export_progress` commands so the
/// frontend can poll progress and abort a long-running export.
#[derive(Debug, Default)]
pub struct ExportState {
    /// 0.0 ..= 1.0 progress of the current export.
    pub progress: Mutex<f32>,
    /// Cancellation token shared with the export executor.
    pub cancel: CancelToken,
}

impl ExportState {
    pub fn reset(&mut self) {
        *self.progress.lock().unwrap() = 0.0;
        self.cancel = CancelToken::new();
    }
}