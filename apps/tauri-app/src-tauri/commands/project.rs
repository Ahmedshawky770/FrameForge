use crate::state::{CompatibilityInfo, ExportState};
use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, ItemId, Segment, SourceId, TimeRange,
    Timebase, TimelineItem, Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::{plan_export, validate_edl};
use frameforge_export_engine::{
    clear_last_encoder_report, disk_space_free, last_encoder_report, output_format_from_extension,
    ExportExecutor, ReencodeExecutor, SmartRenderExecutor, StreamCopyExecutor,
};
use serde::{Deserialize, Serialize};
use serde_json::from_str;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{command, State};

/// Timebase used when importing timestamps from the frontend.
///
/// The frontend sends clip `start`/`end` as plain seconds (f64, e.g. `5.5`).
/// We store them in microsecond timebase so they round-trip losslessly for
/// typical millisecond-precision edits while staying i64-compatible.
const IMPORT_TIMEBASE: Timebase = Timebase::new(1, 1_000_000);

/// Minimum free disk space required before starting an export (10 MiB).
const MIN_FREE_BYTES: u64 = 10 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Import DTOs
//
// The core `Edl` types use `Timestamp { value: i64, tb: Timebase }` and
// `ExportMode`/`Container` enums whose serde representation is PascalCase
// ("StreamCopy", "Mp4"). The frontend sends plain seconds (f64) and lowercase
// strings ("stream-copy", "mp4"). These DTOs bridge the gap so `project_export`
// accepts the frontend payload directly instead of failing serde deserialization.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct ImportEdl {
    pub version: u32,
    pub clips: Vec<ImportClip>,
    pub tracks: Vec<ImportTrack>,
    pub export: ImportExportSettings,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ImportClip {
    pub id: u64,
    pub path: String,
    pub segments: Vec<ImportSegment>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ImportSegment {
    /// Start time in seconds (f64), inclusive.
    pub start: f64,
    /// End time in seconds (f64), exclusive.
    pub end: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ImportTrack {
    pub kind: String, // "video" | "audio" | "subtitle"
    pub enabled: bool,
    pub index: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ImportExportSettings {
    pub mode: String, // "stream-copy" | "reencode"
    pub preset: String,
    pub container: String, // "mp4" | "mkv" | "ts"
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    pub audio_sample_rate: Option<u32>,
    pub audio_channels: Option<u32>,
    pub destination: String,
}

impl ImportEdl {
    /// Convert the import DTO into a core `Edl`, validating segment ranges.
    pub fn into_edl(self) -> Result<Edl, String> {
        let mut clips = Vec::with_capacity(self.clips.len());
        for (clip_idx, clip) in self.clips.into_iter().enumerate() {
            let mut segments = Vec::with_capacity(clip.segments.len());
            for (seg_idx, seg) in clip.segments.into_iter().enumerate() {
                if seg.end <= seg.start {
                    return Err(format!(
                        "clip {} segment {}: end ({}) must be after start ({})",
                        clip_idx, seg_idx, seg.end, seg.start
                    ));
                }
                segments.push(Segment {
                    range: TimeRange::new(
                        seconds_to_timestamp(seg.start),
                        seconds_to_timestamp(seg.end),
                    ),
                    source_id: SourceId(clip.id),
                });
            }
            clips.push(Clip {
                id: SourceId(clip.id),
                path: PathBuf::from(clip.path),
                segments,
            });
        }

        let mode = match self.export.mode.to_lowercase().as_str() {
            "stream-copy" => ExportMode::StreamCopy,
            "reencode" => ExportMode::Reencode,
            other => return Err(format!("unknown export mode: {other}")),
        };

        let container = match self.export.container.to_lowercase().as_str() {
            "mp4" => Container::Mp4,
            "mkv" => Container::Mkv,
            "ts" => Container::Ts,
            other => return Err(format!("unknown container: {other}")),
        };

        let mut tracks: Vec<Track> = self
            .tracks
            .into_iter()
            .enumerate()
            .map(|(i, t)| {
                let kind = match t.kind.to_lowercase().as_str() {
                    "video" => TrackKind::Video,
                    "audio" => TrackKind::Audio,
                    "subtitle" => TrackKind::Subtitle,
                    other => return Err(format!("unknown track kind: {other}")),
                };
                Ok(Track {
                    kind: kind.clone(),
                    enabled: t.enabled,
                    index: t.index.or(Some(i as u32)),
                    name: format!("Track {}", i + 1),
                    height: if matches!(kind, TrackKind::Video) {
                        80.0
                    } else {
                        60.0
                    },
                    muted: false,
                    solo: false,
                    locked: false,
                    items: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        // Derive video-track timeline items from clip segments.  Each segment
        // becomes one TimelineItem placed back-to-back on the timeline so that
        // validate_edl / plan_export see enabled items and can compute cut
        // points.  The source_range keeps the original source in/out points;
        // timeline_start accumulates so items do not overlap.
        if let Some(video_track) = tracks
            .iter_mut()
            .find(|t| matches!(t.kind, TrackKind::Video))
        {
            let mut timeline_cursor = 0.0f64;
            for clip in &clips {
                for seg in &clip.segments {
                    let dur = seg.range.duration_seconds();
                    video_track.items.push(TimelineItem {
                        id: ItemId::default(),
                        source_id: clip.id,
                        name: clip
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "clip".into()),
                        path: clip.path.clone(),
                        source_range: seg.range,
                        timeline_start: seconds_to_timestamp(timeline_cursor),
                        track_index: video_track.index.unwrap_or(0),
                        transition_in: None,
                        transition_out: None,
                        volume: 1.0,
                        opacity: 1.0,
                        enabled: true,
                        locked: false,
                    });
                    timeline_cursor += dur;
                }
            }
        }

        let export = ExportSettings {
            mode,
            preset: self.export.preset,
            container,
            vcodec: self.export.vcodec,
            acodec: self.export.acodec,
            width: self.export.width,
            height: self.export.height,
            fps_num: self.export.fps_num,
            fps_den: self.export.fps_den,
            audio_sample_rate: self.export.audio_sample_rate,
            audio_channels: self.export.audio_channels,
            destination: PathBuf::from(self.export.destination),
        };

        Ok(Edl {
            version: self.version,
            clips,
            tracks,
            export,
        })
    }
}

/// Convert seconds (f64) to a `Timestamp` in the import timebase.
fn seconds_to_timestamp(seconds: f64) -> Timestamp {
    Timestamp::from_seconds(seconds, IMPORT_TIMEBASE)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[command]
pub async fn project_compatibility(clip_paths: Vec<String>) -> Result<CompatibilityInfo, String> {
    // Probe each clip directly (no fabricated 1s durations or placeholder
    // timeline items): analyze_compatibility_paths probes every file once and
    // compares the real video-stream parameters (codec/profile/resolution/fps/
    // timebase/pix_fmt/audio). Missing files or files without a video stream
    // surface as an error naming the offending path.
    let paths: Vec<PathBuf> = clip_paths.into_iter().map(PathBuf::from).collect();

    let report =
        frameforge_edit_engine::analyze_compatibility_paths(&paths).map_err(|e| e.to_string())?;

    Ok(CompatibilityInfo {
        lossless_possible: report.lossless_possible,
        reasons: report.reasons,
        alternative: report.alternative,
    })
}

#[command]
pub async fn project_save(edl_json: String, path: String) -> Result<(), String> {
    std::fs::write(&path, edl_json).map_err(|e| e.to_string())
}

#[command]
pub async fn project_load(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// Run an export with progress reporting, cancellation support and a
/// disk-space precheck.
///
/// Runs the blocking ffmpeg pipeline on `tokio::task::spawn_blocking` so the
/// async command does not starve the runtime while ffmpeg is working. Progress
/// and cancellation are shared via [`ExportState`] so the frontend can poll
/// [`project_export_progress`] and abort with [`project_export_cancel`].
#[command]
pub async fn project_export(
    export_state: State<'_, Arc<Mutex<ExportState>>>,
    edl_json: String,
    output_path: String,
    encoder_pref: String,
    quality_preset: String,
) -> Result<(), String> {
    // 1. Parse EDL using the import DTO (accepts f64 seconds from the frontend).
    let import_edl: ImportEdl =
        from_str(&edl_json).map_err(|e| format!("edl parse failed: {e}"))?;
    let edl = import_edl.into_edl()?;

    // 2. Validate the EDL (non-overlapping segments, monotonic times).
    validate_edl(&edl).map_err(|e| e.to_string())?;

    let mut dest = PathBuf::from(&output_path);
    // Ensure the destination has a recognized video extension so ffmpeg can
    // select a muxer; fall back to mp4 when the user typed an unknown/missing
    // extension instead of failing mid-export (which leaves a stray temp file).
    if output_format_from_extension(&dest).is_none() {
        dest = dest.with_extension("mp4");
    }

    // 3. Disk-space precheck: estimate required free space from the input/output
    // size (sum of source clip sizes) instead of a fixed 10 MiB threshold.
    let estimated_bytes: u64 = edl
        .clips
        .iter()
        .filter_map(|c| std::fs::metadata(&c.path).ok())
        .map(|m| m.len())
        .sum();
    let required_bytes = estimated_bytes.max(MIN_FREE_BYTES);
    let free_bytes = disk_space_free(&dest).map_err(|e| format!("disk space check failed: {e}"))?;
    if free_bytes < required_bytes {
        return Err(format!(
            "insufficient disk space: {} bytes free (need at least {})",
            free_bytes, required_bytes
        ));
    }

    // 4. Plan the export (keyframe snapping + compatibility analysis).
    let plan = plan_export(&edl).map_err(|e| e.to_string())?;
    println!(
        "Export plan: lossless={}, segments={}, clips={}",
        plan.merge_plan.lossless,
        plan.segments.len(),
        edl.clips.len()
    );

    // 5. Reset progress, cancellation token and encoder report for a fresh run.
    clear_last_encoder_report();
    {
        let mut s = export_state.lock().map_err(|e| e.to_string())?;
        s.reset();
    }
    let cancel_token = {
        let s = export_state.lock().map_err(|e| e.to_string())?;
        s.cancel.clone()
    };
    let state_arc = Arc::clone(&*export_state);
    let is_mp4 = matches!(
        output_format_from_extension(&dest),
        Some("mp4") | Some("mov")
    );
    let is_mkv = matches!(output_format_from_extension(&dest), Some("matroska"));
    let use_smart = plan.merge_plan.copy_compatible;

    // 6. Run the blocking ffmpeg pipeline on the blocking thread pool.
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        // Extract normalization targets from the EDL before moving clips.
        let (
            target_width,
            target_height,
            target_fps_num,
            target_fps_den,
            target_audio_rate,
            target_audio_channels,
        ) = (
            edl.export.width,
            edl.export.height,
            edl.export.fps_num,
            edl.export.fps_den,
            edl.export.audio_sample_rate,
            edl.export.audio_channels,
        );

        let clips = edl.clips.clone();
        let encoder_pref = encoder_pref.clone();
        let quality_preset = quality_preset.clone();

        let progress = move |p: f32| {
            if let Ok(mut s) = state_arc.lock() {
                s.progress = p;
            }
        };

        if plan.merge_plan.lossless && !plan.merge_plan.copy_compatible {
            let mut executor = StreamCopyExecutor::new(clips, dest.clone());
            return executor
                .run(&plan, &progress, &cancel_token)
                .map_err(|e| e.to_string());
        }

        if is_mp4 && use_smart {
            frameforge_export_engine::set_last_encoder_report(
                frameforge_export_engine::EncoderReport {
                    requested: encoder_pref.clone(),
                    encoder: "smart copy (copy GOPs + boundary re-encode)".to_string(),
                    hardware: false,
                    fallback_reason: None,
                },
            );
            let mut executor = SmartRenderExecutor::new(
                clips.clone(),
                dest.clone(),
                encoder_pref.clone(),
                quality_preset.clone(),
            )
            .with_normalization(
                target_width,
                target_height,
                target_fps_num,
                target_fps_den,
                None,
            target_audio_rate,
            target_audio_channels,
        );
            return match executor.run(&plan, &progress, &cancel_token) {
                Ok(()) => Ok(()),
                Err(frameforge_core_types::FrameForgeError::SmartRenderUnsupported(reason)) => {
                    eprintln!(
                        "smart render unsupported ({reason}); falling back to full re-encode"
                    );
                    clear_last_encoder_report();
                    let mut fallback = SmartRenderExecutor::new(
                        clips,
                        dest.clone(),
                        encoder_pref,
                        quality_preset,
                    )
                    .with_normalization(
                        target_width,
                        target_height,
                        target_fps_num,
                        target_fps_den,
                        None,
                        target_audio_rate,
                        target_audio_channels,
                    )
                    .with_force_reencode(true);
                    fallback
                        .run(&plan, &progress, &cancel_token)
                        .map_err(|e| e.to_string())
                }
                Err(e) => Err(e.to_string()),
            };
        }

        if is_mp4 || is_mkv {
            let mut executor = SmartRenderExecutor::new(
                clips,
                dest.clone(),
                encoder_pref,
                quality_preset,
            )
            .with_normalization(
                target_width,
                target_height,
                target_fps_num,
                target_fps_den,
                None,
                target_audio_rate,
                target_audio_channels,
            )
            .with_force_reencode(true);
            return executor
                .run(&plan, &progress, &cancel_token)
                .map_err(|e| e.to_string());
        }

        let mut executor = ReencodeExecutor::new(
            clips,
            dest.clone(),
            encoder_pref,
            quality_preset,
        )
        .with_normalization(
            target_width,
            target_height,
            target_fps_num,
            target_fps_den,
            None,
            target_audio_rate,
            target_audio_channels,
        );

        executor
            .run(&plan, &progress, &cancel_token)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("export task failed: {e}"))??;

    // 7. Record which encoder actually ran so the UI can show it (hardware
    // preferences can silently degrade to software).
    let encoder_label = match last_encoder_report() {
        Some(report) => match report.fallback_reason {
            Some(reason) => format!("{} ({reason})", report.encoder),
            None => report.encoder,
        },
        // The stream-copy executor never resolves an encoder.
        None => "copy (lossless stream copy)".to_string(),
    };
    {
        let mut s = export_state.lock().map_err(|e| e.to_string())?;
        s.last_encoder = Some(encoder_label);
    }

    Ok(())
}

/// Abort the currently running export (if any).
#[command]
pub async fn project_export_cancel(
    export_state: State<'_, Arc<Mutex<ExportState>>>,
) -> Result<(), String> {
    let s = export_state.lock().map_err(|e| e.to_string())?;
    s.cancel.cancel();
    Ok(())
}

/// Return the current export progress (0.0 ..= 1.0).
#[command]
pub async fn project_export_progress(
    export_state: State<'_, Arc<Mutex<ExportState>>>,
) -> Result<f32, String> {
    let s = export_state.lock().map_err(|e| e.to_string())?;
    Ok(s.progress)
}

/// Describe the encoder used by the most recent export, if any.
#[command]
pub async fn project_export_encoder(
    export_state: State<'_, Arc<Mutex<ExportState>>>,
) -> Result<Option<String>, String> {
    let s = export_state.lock().map_err(|e| e.to_string())?;
    Ok(s.last_encoder.clone())
}

/// Open a save file dialog for choosing the export destination.
#[command]
pub async fn save_file_dialog() -> Result<Option<String>, String> {
    let file = rfd::AsyncFileDialog::new()
        .add_filter("Video", &["mp4", "mkv", "webm", "mov", "ts", "avi"])
        .add_filter("All", &["*"])
        .set_file_name("output.mp4")
        .save_file()
        .await;
    Ok(file.map(|f| {
        let mut p = f.path().to_string_lossy().to_string();
        let path = std::path::Path::new(&p);
        let has_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| !e.is_empty())
            .unwrap_or(false);
        if !has_ext {
            p.push_str(".mp4");
        }
        p
    }))
}

/// Open a save file dialog for choosing the project save location.
#[command]
pub async fn save_project_dialog() -> Result<Option<String>, String> {
    let file = rfd::AsyncFileDialog::new()
        .add_filter("FrameForge Project", &["json"])
        .add_filter("All", &["*"])
        .set_file_name("project.json")
        .save_file()
        .await;
    Ok(file.map(|f| f.path().to_string_lossy().to_string()))
}

/// Open a file dialog for choosing a project to load.
#[command]
pub async fn load_project_dialog() -> Result<Option<String>, String> {
    let file = rfd::AsyncFileDialog::new()
        .add_filter("FrameForge Project", &["json"])
        .add_filter("All", &["*"])
        .pick_file()
        .await;
    Ok(file.map(|f| f.path().to_string_lossy().to_string()))
}

#[command]
pub async fn open_file_dialog() -> Result<Option<String>, String> {
    let files = rfd::AsyncFileDialog::new()
        .add_filter("Video", &["mp4", "mkv", "webm", "mov", "ts", "avi"])
        .add_filter("All", &["*"])
        .pick_file()
        .await;
    Ok(files.map(|f| f.path().to_string_lossy().to_string()))
}

/// Open a multi-file dialog for choosing one or more video files (merge workflow).
#[command]
pub async fn open_video_files_dialog() -> Result<Option<Vec<String>>, String> {
    let files = rfd::AsyncFileDialog::new()
        .add_filter("Video", &["mp4", "mkv", "webm", "mov", "ts", "avi"])
        .add_filter("All", &["*"])
        .pick_files()
        .await;
    Ok(files.map(|files| {
        files
            .iter()
            .map(|f| f.path().to_string_lossy().to_string())
            .collect()
    }))
}

/// Probe a media file and return its duration in seconds.
#[command]
pub async fn media_probe_duration(path: String) -> Result<Option<f64>, String> {
    frameforge_media_probe::probe(Path::new(&path))
        .map(|r| r.format.duration_sec)
        .map_err(|e| e.to_string())
}

/// Delete ranges from a single video: given the source path, the full duration,
/// and a list of [start, end) ranges to remove, returns an EDL JSON string
/// whose single clip contains the remaining (non-deleted) segments.
#[command]
pub async fn project_delete_ranges(
    source_path: Option<String>,
    duration: f64,
    delete_ranges: Vec<(f64, f64)>,
) -> Result<String, String> {
    let source_path =
        source_path.ok_or_else(|| "source_path is required — load a video first".to_string())?;
    if source_path.is_empty() {
        return Err("source_path is empty — load a video first".into());
    }
    if duration <= 0.0 {
        return Err("duration must be greater than zero".into());
    }
    // Sort and merge overlapping delete ranges
    let mut ranges: Vec<(f64, f64)> = delete_ranges
        .iter()
        .filter(|(s, e)| e > s && *s >= 0.0 && *e <= duration)
        .cloned()
        .collect();
    ranges.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // Merge overlapping ranges
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (s, e) in ranges {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        merged.push((s, e));
    }

    // Build remaining segments
    let mut segments: Vec<ImportSegment> = Vec::new();
    let mut cursor = 0.0f64;
    for (s, e) in &merged {
        if *s > cursor {
            segments.push(ImportSegment {
                start: cursor,
                end: *s,
            });
        }
        cursor = cursor.max(*e);
    }
    if cursor < duration {
        segments.push(ImportSegment {
            start: cursor,
            end: duration,
        });
    }

    // If everything was deleted, return a clear error instead of letting
    // validate_edl surface a generic "No enabled items in any track".
    if segments.is_empty() {
        return Err("Export failed: all ranges deleted — nothing remains to export".into());
    }

    let edl = ImportEdl {
        version: 1,
        clips: vec![ImportClip {
            id: 1,
            path: source_path,
            segments,
        }],
        tracks: vec![
            ImportTrack {
                kind: "video".into(),
                enabled: true,
                index: Some(0),
            },
            ImportTrack {
                kind: "audio".into(),
                enabled: true,
                index: Some(1),
            },
        ],
        export: ImportExportSettings {
            mode: "stream-copy".into(),
            preset: "fast".into(),
            container: "mp4".into(),
            vcodec: None,
            acodec: None,
            width: None,
            height: None,
            fps_num: None,
            fps_den: None,
            audio_sample_rate: None,
            audio_channels: None,
            destination: String::new(),
        },
    };

    serde_json::to_string(&edl).map_err(|e| format!("serialize edl: {e}"))
}

// ---------------------------------------------------------------------------
// Tests for the EDL import DTO (Gap 1 fix: f64 seconds -> Timestamp)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use frameforge_core_types::Timebase;

    fn tb() -> Timebase {
        Timebase::new(1, 1_000_000)
    }

    fn sample_import_edl() -> ImportEdl {
        ImportEdl {
            version: 1,
            clips: vec![
                ImportClip {
                    id: 1,
                    path: "/tmp/a.mp4".into(),
                    segments: vec![
                        ImportSegment {
                            start: 0.0,
                            end: 5.5,
                        },
                        ImportSegment {
                            start: 10.25,
                            end: 15.75,
                        },
                    ],
                },
                ImportClip {
                    id: 2,
                    path: "/tmp/b.mp4".into(),
                    segments: vec![ImportSegment {
                        start: 2.0,
                        end: 8.0,
                    }],
                },
            ],
            tracks: vec![
                ImportTrack {
                    kind: "video".into(),
                    enabled: true,
                    index: Some(0),
                },
                ImportTrack {
                    kind: "audio".into(),
                    enabled: true,
                    index: Some(1),
                },
            ],
            export: ImportExportSettings {
                mode: "stream-copy".into(),
                preset: "fast".into(),
                container: "mp4".into(),
                vcodec: None,
                acodec: None,
                width: None,
                height: None,
                fps_num: None,
                fps_den: None,
                audio_sample_rate: None,
                audio_channels: None,
                destination: "/tmp/out.mp4".into(),
            },
        }
    }

    #[test]
    fn seconds_to_timestamp_converts_seconds_to_microsecond_tb() {
        let t = seconds_to_timestamp(5.5);
        assert_eq!(t, Timestamp::new(5_500_000, tb()));
        let t0 = seconds_to_timestamp(0.0);
        assert_eq!(t0, Timestamp::new(0, tb()));
        // Round-trip via to_seconds is lossy but close for ms precision.
        assert!((seconds_to_timestamp(0.1).to_seconds() - 0.1).abs() < 1e-4);
    }

    #[test]
    fn import_edl_into_edl_valid() {
        let edl = sample_import_edl()
            .into_edl()
            .expect("valid import should convert");

        assert_eq!(edl.version, 1);
        assert_eq!(edl.clips.len(), 2);

        // Clip 1: two segments with microsecond timestamps.
        assert_eq!(edl.clips[0].id, SourceId(1));
        assert_eq!(edl.clips[0].path, PathBuf::from("/tmp/a.mp4"));
        assert_eq!(edl.clips[0].segments.len(), 2);
        assert_eq!(
            edl.clips[0].segments[0].range,
            TimeRange::new(Timestamp::new(0, tb()), Timestamp::new(5_500_000, tb()))
        );
        assert_eq!(
            edl.clips[0].segments[1].range,
            TimeRange::new(
                Timestamp::new(10_250_000, tb()),
                Timestamp::new(15_750_000, tb())
            )
        );

        // Clip 2: single segment.
        assert_eq!(edl.clips[1].id, SourceId(2));
        assert_eq!(edl.clips[1].segments.len(), 1);
        assert_eq!(
            edl.clips[1].segments[0].range,
            TimeRange::new(
                Timestamp::new(2_000_000, tb()),
                Timestamp::new(8_000_000, tb())
            )
        );

        // Export settings mapped from lowercase strings.
        assert_eq!(edl.export.mode, ExportMode::StreamCopy);
        assert_eq!(edl.export.container, Container::Mp4);
        assert_eq!(edl.export.destination, PathBuf::from("/tmp/out.mp4"));
        assert_eq!(edl.tracks.len(), 2);
        assert_eq!(edl.tracks[0].kind, TrackKind::Video);
        assert_eq!(edl.tracks[1].kind, TrackKind::Audio);
    }

    #[test]
    fn import_edl_rejects_end_before_start() {
        let mut edl = sample_import_edl();
        edl.clips[0].segments[0] = ImportSegment {
            start: 5.0,
            end: 2.0,
        };
        let err = edl.into_edl().unwrap_err();
        assert!(err.contains("end"), "error should mention end: {err}");
    }

    #[test]
    fn import_edl_rejects_equal_start_and_end() {
        let mut edl = sample_import_edl();
        edl.clips[0].segments[0] = ImportSegment {
            start: 3.0,
            end: 3.0,
        };
        assert!(edl.into_edl().is_err());
    }

    #[test]
    fn import_edl_rejects_unknown_mode() {
        let mut edl = sample_import_edl();
        edl.export.mode = "copy-only".into();
        assert!(edl.into_edl().is_err());
    }

    #[test]
    fn import_edl_rejects_unknown_container() {
        let mut edl = sample_import_edl();
        edl.export.container = "avi".into();
        assert!(edl.into_edl().is_err());
    }

    #[test]
    fn import_edl_json_round_trip() {
        let edl = sample_import_edl();
        let json = serde_json::to_string(&edl).expect("serialize import edl");
        let parsed: ImportEdl = from_str(&json).expect("deserialize import edl");
        let converted = parsed.into_edl().expect("convert round-tripped edl");
        assert_eq!(converted.clips.len(), 2);
        assert_eq!(converted.export.mode, ExportMode::StreamCopy);
    }

    #[test]
    fn import_edl_tracks_mapped_correctly() {
        let mut edl = sample_import_edl();
        edl.tracks = vec![
            ImportTrack {
                kind: "subtitle".into(),
                enabled: true,
                index: None,
            },
            ImportTrack {
                kind: "VIDEO".into(),
                enabled: false,
                index: Some(5),
            },
        ];
        let converted = edl
            .into_edl()
            .expect("tracks should convert case-insensitively");
        assert_eq!(converted.tracks[0].kind, TrackKind::Subtitle);
        assert!(converted.tracks[0].enabled);
        assert_eq!(converted.tracks[1].kind, TrackKind::Video);
        assert!(!converted.tracks[1].enabled);
        assert_eq!(converted.tracks[1].index, Some(5));
    }

    #[test]
    fn delete_ranges_derive_track_items_and_validate() {
        // Simulate project_delete_ranges output: 10s video, delete middle 3-7s
        // -> remaining segments: 0-3s and 7-10s
        let edl = ImportEdl {
            version: 1,
            clips: vec![ImportClip {
                id: 1,
                path: "/tmp/video.mp4".into(),
                segments: vec![
                    ImportSegment {
                        start: 0.0,
                        end: 3.0,
                    },
                    ImportSegment {
                        start: 7.0,
                        end: 10.0,
                    },
                ],
            }],
            tracks: vec![
                ImportTrack {
                    kind: "video".into(),
                    enabled: true,
                    index: Some(0),
                },
                ImportTrack {
                    kind: "audio".into(),
                    enabled: true,
                    index: Some(1),
                },
            ],
            export: ImportExportSettings {
                mode: "stream-copy".into(),
                preset: "fast".into(),
                container: "mp4".into(),
                vcodec: None,
                acodec: None,
                width: None,
                height: None,
                fps_num: None,
                fps_den: None,
                audio_sample_rate: None,
                audio_channels: None,
                destination: "/tmp/out.mp4".into(),
            },
        };

        let converted = edl.into_edl().expect("convert should succeed");

        // Video track should have 2 items (one per remaining segment).
        assert_eq!(converted.tracks.len(), 2);
        let video_items: Vec<_> = converted
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Video))
            .flat_map(|t| t.items.iter())
            .collect();
        assert_eq!(
            video_items.len(),
            2,
            "video track should have 2 items for 2 remaining segments"
        );

        // Timeline items placed back-to-back: 0-3s then 3-6s (not source 7-10s on timeline).
        assert_eq!(video_items[0].timeline_start.to_seconds(), 0.0);
        assert_eq!(video_items[1].timeline_start.to_seconds(), 3.0);
        // Source ranges preserved: 0-3s and 7-10s.
        assert_eq!(video_items[0].source_range.start.to_seconds(), 0.0);
        assert_eq!(video_items[0].source_range.end.to_seconds(), 3.0);
        assert_eq!(video_items[1].source_range.start.to_seconds(), 7.0);
        assert_eq!(video_items[1].source_range.end.to_seconds(), 10.0);

        // Total exported duration = 3s + 3s = 6s = 10s original - 4s deleted.
        let total_duration: f64 = video_items
            .iter()
            .map(|i| i.source_range.duration_seconds())
            .sum();
        assert!(
            (total_duration - 6.0).abs() < 1e-6,
            "total duration should be 6s, got {total_duration}"
        );

        // validate_edl should pass (enabled items exist on video track).
        validate_edl(&converted).expect("validate_edl should pass for valid delete-ranges EDL");
    }
}
