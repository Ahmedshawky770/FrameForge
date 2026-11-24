//! End-to-end coverage for the cached keyframe index.
//!
//! Building an index reads the file's packet table, so an N-segment plan must
//! reuse one index per file instead of rescanning per segment.

use frameforge_media_probe::{build_keyframe_index_cached, clear_keyframe_index_cache};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

fn gen(path: &PathBuf) {
    let st = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=6:size=160x120:rate=25",
            "-c:v",
            "libx264",
            "-g",
            "25",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
}

#[test]
fn cached_index_is_reused_and_sorted() {
    clear_keyframe_index_cache();

    let path = std::env::temp_dir().join(format!("frameforge_kf_e2e_{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&path);
    gen(&path);

    // First call scans the file.
    let first = build_keyframe_index_cached(&path, Some(3.0)).expect("index");
    assert!(!first.entries.is_empty(), "test video must have keyframes");

    // Second call for the same file and an equal/lower limit must reuse it.
    let second = build_keyframe_index_cached(&path, Some(2.0)).expect("index");
    assert!(
        Arc::ptr_eq(&first, &second),
        "cached index should be reused for the same file"
    );

    // Entries are sorted by pts (required for the binary search).
    for w in first.entries.windows(2) {
        assert!(
            w[0].pts.to_seconds() <= w[1].pts.to_seconds(),
            "keyframe entries must be sorted by pts"
        );
    }

    // A larger limit forces a rescan (the cached index is too short).
    let longer = build_keyframe_index_cached(&path, Some(6.0)).expect("index");
    assert!(!Arc::ptr_eq(&first, &longer));

    clear_keyframe_index_cache();
    let _ = std::fs::remove_file(&path);
}
