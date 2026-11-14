//! Headless CLI: probe, build-index, diagnose-hw, export smoke.

use clap::{Parser, Subcommand};
use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, FrameForgeError, Segment, SourceId,
    TimeRange, Timebase, Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::{plan_export, validate_edl};
use frameforge_export_engine::{disk_space_free, ExportExecutor, StreamCopyExecutor};
use frameforge_hw_diagnostics::probe_hw;
use serde_json::{from_str, to_string_pretty};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "frameforge-cli", about = "FrameForge headless tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Probe a media file and print stream/format info.
    Probe { path: PathBuf },
    /// Build a keyframe index for a media file.
    BuildIndex { path: PathBuf },
    /// Print hardware diagnostics.
    DiagnoseHw,
    /// Validate an EDL JSON file.
    ValidateEdl { path: PathBuf },
    /// Save an EDL to a JSON file.
    SaveEdl {
        /// Input media file path
        #[arg(long)]
        input: PathBuf,
        /// Output EDL JSON file path
        #[arg(long)]
        output: PathBuf,
        /// Segments as start,end seconds pairs (e.g. "0,5 10,15")
        #[arg(long)]
        segments: String,
    },
    /// Load an EDL JSON file and print summary.
    LoadEdl { path: PathBuf },
    /// Check free disk space at a path.
    DiskSpace { path: PathBuf },
    /// Export (stream-copy) segments from an input file to an output file.
    /// --clips specifies input file path; --segments specifies start,end pairs in seconds.
    Export {
        /// Input media file path
        #[arg(long)]
        input: PathBuf,
        /// Output media file path
        #[arg(long)]
        output: PathBuf,
        /// Segments as start,end seconds pairs (e.g. "0,5 10,15")
        #[arg(long)]
        segments: Option<String>,
        /// Load EDL from a JSON file instead of --segments
        #[arg(long)]
        edl: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli.cmd) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cmd: Commands) -> Result<(), FrameForgeError> {
    match cmd {
        Commands::Probe { path } => {
            let result = frameforge_media_probe::probe(&path)?;
            println!("=== Probe: {:?} ===", path);
            println!("Format: {}", result.format.format_name);
            if let Some(d) = result.format.duration_sec {
                println!("Duration: {:.3} s", d);
            }
            if let Some(br) = result.format.bit_rate {
                println!("Bit rate: {} bps", br);
            }
            println!("\nStreams:");
            for s in &result.streams {
                println!(
                    "  [{}] {:?} codec={} {:?} {}",
                    s.index,
                    s.kind,
                    s.codec,
                    s.codec_type,
                    match (s.width, s.height) {
                        (Some(w), Some(h)) => format!("{}x{}", w, h),
                        _ => "".into(),
                    }
                );
                if let Some(fps_num) = s.fps_num {
                    if let Some(fps_den) = s.fps_den {
                        if fps_den > 0 {
                            println!("      fps={:.3}", fps_num as f64 / fps_den as f64);
                        }
                    }
                }
                println!("      timebase={}/{}", s.timebase_num, s.timebase_den);
            }
            Ok(())
        }
        Commands::BuildIndex { path } => {
            let idx = frameforge_media_probe::build_keyframe_index(&path)?;
            println!("=== Keyframe Index: {:?} ===", path);
            println!("Keyframes: {}", idx.entries.len());
            for (i, entry) in idx.entries.iter().enumerate().take(20) {
                println!(
                    "  {}: pts={}@{}/{} offset={}",
                    i, entry.pts.value, entry.pts.tb.num, entry.pts.tb.den, entry.file_offset
                );
            }
            if idx.entries.len() > 20 {
                println!("  ... and {} more", idx.entries.len() - 20);
            }
            Ok(())
        }
        Commands::DiagnoseHw => {
            let d = probe_hw();
            println!("{:#?}", d);
            Ok(())
        }
        Commands::ValidateEdl { path } => {
            let json = std::fs::read_to_string(&path)?;
            let edl: Edl = serde_json::from_str(&json)?;
            validate_edl(&edl)?;
            println!("edl valid: {path:?}");
            Ok(())
        }
        Commands::SaveEdl {
            input,
            output,
            segments,
        } => {
            let seg_pairs: Vec<(f64, f64)> = segments
                .split_whitespace()
                .map(|pair| {
                    let parts: Vec<f64> = pair
                        .split(',')
                        .map(|s| s.parse::<f64>().expect("invalid number"))
                        .collect();
                    if parts.len() != 2 {
                        panic!("invalid segment format: {pair} (expected start,end)");
                    }
                    (parts[0], parts[1])
                })
                .collect();

            if seg_pairs.is_empty() {
                return Err(FrameForgeError::Export("no segments specified".into()));
            }

            let tb = Timebase::new(1, 1_000_000);
            let mut edl_segments = Vec::new();

            for (start_s, end_s) in seg_pairs.iter() {
                let start_ts = Timestamp::from_seconds(*start_s, tb);
                let end_ts = Timestamp::from_seconds(*end_s, tb);
                edl_segments.push(Segment {
                    range: TimeRange::new(start_ts, end_ts),
                    source_id: SourceId(1),
                });
            }

            let edl = Edl {
                version: 1,
                clips: vec![Clip {
                    id: SourceId(1),
                    path: input.clone(),
                    segments: edl_segments,
                }],
                tracks: vec![
                    Track {
                        kind: TrackKind::Video,
                        enabled: true,
                        index: Some(0),
                        name: "Video 1".into(),
                        height: 80.0,
                        muted: false,
                        solo: false,
                        locked: false,
                        items: Vec::new(),
                    },
                    Track {
                        kind: TrackKind::Audio,
                        enabled: true,
                        index: Some(1),
                        name: "Audio 1".into(),
                        height: 60.0,
                        muted: false,
                        solo: false,
                        locked: false,
                        items: Vec::new(),
                    },
                ],
                export: ExportSettings {
                    mode: ExportMode::StreamCopy,
                    preset: "fast".into(),
                    container: Container::Mp4,
                    vcodec: None,
                    acodec: None,
                    width: None,
                    height: None,
                    fps_num: None,
                    fps_den: None,
                    audio_sample_rate: None,
                    audio_channels: None,
                    destination: output.clone(),
                },
            };

            validate_edl(&edl)?;
            let json = to_string_pretty(&edl)
                .map_err(|e| FrameForgeError::Export(format!("edl serialize failed: {e}")))?;
            std::fs::write(&output, json).map_err(FrameForgeError::Io)?;
            println!("EDL saved: {:?}", output);
            Ok(())
        }
        Commands::LoadEdl { path } => {
            let json = std::fs::read_to_string(&path)?;
            let edl: Edl = serde_json::from_str(&json)?;
            validate_edl(&edl)?;
            println!("=== EDL: {:?} ===", path);
            println!("Version: {}", edl.version);
            println!("Clips: {}", edl.clips.len());
            for clip in &edl.clips {
                println!(
                    "  Clip {}: {} ({} segments)",
                    clip.id.0,
                    clip.path.display(),
                    clip.segments.len()
                );
                for seg in &clip.segments {
                    println!(
                        "    [{:.3} - {:3} s]",
                        seg.range.start.to_seconds(),
                        seg.range.end.to_seconds()
                    );
                }
            }
            println!(
                "Export: mode={:?}, container={:?}, dest={:?}",
                edl.export.mode, edl.export.container, edl.export.destination
            );
            Ok(())
        }
        Commands::DiskSpace { path } => {
            let free = disk_space_free(&path)?;
            println!("free bytes approx: {free}");
            Ok(())
        }
        Commands::Export {
            input,
            output,
            segments,
            edl,
        } => {
            let edl = if let Some(edl_path) = edl {
                let json = std::fs::read_to_string(&edl_path)?;
                from_str(&json)
                    .map_err(|e| FrameForgeError::Export(format!("edl parse failed: {e}")))?
            } else {
                let seg_pairs: Vec<(f64, f64)> = segments
                    .ok_or_else(|| {
                        FrameForgeError::Export("either --segments or --edl is required".into())
                    })?
                    .split_whitespace()
                    .map(|pair| {
                        let parts: Vec<f64> = pair
                            .split(',')
                            .map(|s| s.parse::<f64>().expect("invalid number"))
                            .collect();
                        if parts.len() != 2 {
                            panic!("invalid segment format: {pair} (expected start,end)");
                        }
                        (parts[0], parts[1])
                    })
                    .collect();

                if seg_pairs.is_empty() {
                    return Err(FrameForgeError::Export("no segments specified".into()));
                }

                let tb = Timebase::new(1, 1_000_000);
                let mut edl_segments = Vec::new();

                for (start_s, end_s) in seg_pairs.iter() {
                    let start_ts = Timestamp::from_seconds(*start_s, tb);
                    let end_ts = Timestamp::from_seconds(*end_s, tb);
                    edl_segments.push(Segment {
                        range: TimeRange::new(start_ts, end_ts),
                        source_id: SourceId(1),
                    });
                }

                Edl {
                    version: 1,
                    clips: vec![Clip {
                        id: SourceId(1),
                        path: input.clone(),
                        segments: edl_segments,
                    }],
                    tracks: vec![
                        Track {
                            kind: TrackKind::Video,
                            enabled: true,
                            index: Some(0),
                            name: "Video 1".into(),
                            height: 80.0,
                            muted: false,
                            solo: false,
                            locked: false,
                            items: Vec::new(),
                        },
                        Track {
                            kind: TrackKind::Audio,
                            enabled: true,
                            index: Some(1),
                            name: "Audio 1".into(),
                            height: 60.0,
                            muted: false,
                            solo: false,
                            locked: false,
                            items: Vec::new(),
                        },
                    ],
                    export: ExportSettings {
                        mode: ExportMode::StreamCopy,
                        preset: "fast".into(),
                        container: Container::Mp4,
                        vcodec: None,
                        acodec: None,
                        width: None,
                        height: None,
                        fps_num: None,
                        fps_den: None,
                        audio_sample_rate: None,
                        audio_channels: None,
                        destination: output.clone(),
                    },
                }
            };

            validate_edl(&edl)?;

            // Plan the export.
            let plan = plan_export(&edl)?;

            println!("=== Export Plan ===");
            println!("Lossless possible: {}", plan.merge_plan.lossless);
            if !plan.merge_plan.reasons.is_empty() {
                println!("Reasons:");
                for r in &plan.merge_plan.reasons {
                    println!("  - {r}");
                }
            }
            println!("Segments: {}", plan.segments.len());

            // Run stream-copy export.
            let mut executor = StreamCopyExecutor::new(edl.clips, output.clone());
            let progress = |_p: f32| {};
            let cancel = frameforge_export_engine::CancelToken::new();
            executor.run(&plan, &progress, &cancel)?;

            println!("Export complete: {:?}", output);
            Ok(())
        }
    }
}
