//! Integration coverage for hardware encoder preferences.
//!
//! These run on any machine: a hardware preference either encodes for real or
//! degrades to software with an explicit reason. Both outcomes must produce a
//! playable file — never a failed export.

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_export_engine::{
    last_encoder_report, CancelToken, ExportExecutor, ReencodeExecutor,
};
use std::process::Command;

fn tb() -> Timebase {
    Timebase::new(1, 1_000_000)
}
fn ts(s: f64) -> Timestamp {
    Timestamp::from_seconds(s, tb())
}

fn gen(path: &std::path::Path) {
    let st = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=3:size=320x240:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:duration=3",
            "-shortest",
            "-c:v",
            "libx264",
            "-g",
            "30",
            "-bf",
            "0",
            "-c:a",
            "aac",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
}

fn export_with_encoder(pref: &str) {
    let dir = std::env::temp_dir();
    let tag = pref.replace(|c: char| !c.is_ascii_alphanumeric(), "_");
    let src = dir.join(format!("hw_{tag}_src_{}.mp4", std::process::id()));
    let out = dir.join(format!("hw_{tag}_out_{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
    gen(&src);

    let edl = Edl {
        version: 1,
        clips: vec![Clip {
            id: SourceId(1),
            path: src.clone(),
            segments: vec![Segment {
                range: TimeRange::new(ts(0.0), ts(2.0)),
                source_id: SourceId(1),
            }],
        }],
        tracks: vec![Track {
            kind: TrackKind::Video,
            enabled: true,
            index: Some(0),
            name: "V".into(),
            height: 80.0,
            muted: false,
            solo: false,
            locked: false,
            items: vec![],
        }],
        export: ExportSettings {
            mode: ExportMode::Reencode,
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
            destination: out.clone(),
        },
    };

    frameforge_edit_engine::validate_edl(&edl).unwrap();
    let plan = frameforge_edit_engine::plan_export(&edl).unwrap();
    let mut exec =
        ReencodeExecutor::new(edl.clips.clone(), out.clone(), pref.into(), "fast".into());
    let cancel = CancelToken::new();
    exec.run(&plan, &|_| {}, &cancel)
        .unwrap_or_else(|e| panic!("encoder preference '{pref}' should export or fall back: {e}"));

    let meta = std::fs::metadata(&out).expect("output exists");
    assert!(
        meta.len() > 1000,
        "'{pref}' produced a suspiciously small file"
    );

    let report = last_encoder_report().unwrap_or_else(|| panic!("no report for '{pref}'"));
    assert_eq!(report.requested, pref);
    let hw_requested = matches!(pref, "vaapi" | "nvenc");
    if report.hardware {
        assert!(
            report.fallback_reason.is_none(),
            "hardware report must not carry a fallback reason"
        );
        assert!(
            report.encoder.ends_with("nvenc") || report.encoder.ends_with("vaapi"),
            "unexpected hardware encoder name: {}",
            report.encoder
        );
    } else if hw_requested {
        assert!(
            report.fallback_reason.is_some(),
            "software fallback for '{pref}' must explain why"
        );
    } else {
        assert!(
            report.fallback_reason.is_none(),
            "a software preference must not be reported as a fallback"
        );
    }

    eprintln!("{pref}: {} bytes via {}", meta.len(), report.encoder);

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn hardware_preferences_export_or_degrade_with_reason() {
    // Single test so the process-wide encoder report is not shared between
    // concurrently running cases.
    export_with_encoder("vaapi");
    export_with_encoder("nvenc");
    export_with_encoder("x264");
}
