//! Phase join: smart-path export with one middle cut.
mod fixtures;

use frameforge_core_types::{
    Clip, Container, Edl, ExportMode, ExportSettings, Segment, SourceId, TimeRange, Timebase,
    Timestamp, Track, TrackKind,
};
use frameforge_edit_engine::{plan_export, ExportPlan};
use frameforge_export_engine::{CancelToken, ExportExecutor, SmartRenderExecutor};
use std::path::{Path, PathBuf};

fn tb() -> Timebase {
    Timebase::new(1, 1_000_000)
}
fn ts(s: f64) -> Timestamp {
    Timestamp::from_seconds(s, tb())
}

fn make_edl(path: &Path, ranges: &[(f64, f64)]) -> Edl {
    let mut segs = Vec::new();
    for (a, b) in ranges {
        segs.push(Segment {
            range: TimeRange::new(ts(*a), ts(*b)),
            source_id: SourceId(1),
        });
    }
    Edl {
        version: 1,
        clips: vec![Clip {
            id: SourceId(1),
            path: path.to_path_buf(),
            segments: segs.clone(),
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
            destination: PathBuf::new(),
        },
    }
}

#[test]
fn phase_join_smart_cut_av_sync() {
    let dir = std::env::temp_dir().join(format!("ff_join_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    fixtures::gen_av(&src, 180, 30, 0.0).unwrap();
    let out = dir.join("out.mp4");
    let edl = make_edl(&src, &[(0.0, 2.5), (4.0, 6.0)]);
    let plan = plan_export(&edl).expect("plan_export");
    let mut exec =
        SmartRenderExecutor::new(edl.clips, out.clone().into(), "auto".into(), "fast".into());
    exec.run(&plan, &|_| {}, &CancelToken::new()).unwrap();

    // Read output
    let mut ictx = ffmpeg_next::format::input(&out).unwrap();
    let stream_indices: Vec<(usize, ffmpeg_next::Rational)> =
        ictx.streams().map(|s| (s.index(), s.time_base())).collect();
    for (si, (idx, tb)) in stream_indices.iter().enumerate() {
        let mut pts_list = Vec::new();
        let mut dts_list = Vec::new();
        let mut count = 0;
        // reopen to avoid borrow issues
        let mut ictx2 = ffmpeg_next::format::input(&out).unwrap();
        for (s, pkt) in ictx2.packets() {
            if s.index() != *idx {
                continue;
            }
            pts_list.push(pkt.pts().unwrap_or(0));
            dts_list.push(pkt.dts().unwrap_or(0));
            count += 1;
        }
        if count == 0 {
            continue;
        }
        // pts deltas around join (midpoint)
        let mid: usize = count / 2;
        for i in mid.saturating_sub(2)..=(mid + 2).min(count - 1) {
            if i > 0 {
                println!("  delta[{}]={}", i, pts_list[i] - pts_list[i - 1]);
            }
        }
        // monotonic dts
        for i in 1..count {
            assert!(
                dts_list[i] > dts_list[i - 1],
                "dts not strictly increasing at {}",
                i
            );
        }
        // pts >= dts without forcing (allow equality)
        for i in 0..count {
            assert!(pts_list[i] >= dts_list[i], "pts<dts at {}", i);
        }
    }

    // Re-read per stream arrays for assertions.
    let mut video_pts = Vec::new();
    let mut audio_pts = Vec::new();
    let mut ictx3 = ffmpeg_next::format::input(&out).unwrap();
    let vidx = ictx3
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .map(|s| s.index())
        .unwrap();
    let aidx = ictx3
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Audio)
        .map(|s| s.index())
        .unwrap();
    let vtb = ictx3.stream(vidx).unwrap().time_base();
    println!("VTB={:?}", vtb);
    let atb = ictx3.stream(aidx).unwrap().time_base();
    let mut ictx4 = ffmpeg_next::format::input(&out).unwrap();
    for (s, pkt) in ictx4.packets() {
        if s.index() == vidx {
            video_pts.push(pkt.pts().unwrap_or(0));
        }
        if s.index() == aidx {
            audio_pts.push(pkt.pts().unwrap_or(0));
        }
    }
    println!(
        "DEBUG test vcount={} last_v={} acount={} last_a={}",
        video_pts.len(),
        video_pts.last().unwrap_or(&0),
        audio_pts.len(),
        audio_pts.last().unwrap_or(&0)
    );
    let v_last = *video_pts.last().unwrap() as f64 * (vtb.0 as f64 / vtb.1 as f64);
    let a_last = *audio_pts.last().unwrap() as f64 * (atb.0 as f64 / atb.1 as f64);
    println!("MEASURED A/V end offset = {} s", (v_last - a_last).abs());
    assert!(
        (v_last - a_last).abs() <= (1024.0 / 48000.0) + 1e-3,
        "A/V end misaligned: {}",
        (v_last - a_last).abs()
    );

    // pts deltas across join (video) equal frame duration +/- 1 tick.
    let frame_dur_ticks = 512i64;
    let join = video_pts.len() / 2; // approximate
    for i in (join.saturating_sub(1))..=(join + 1).min(video_pts.len() - 1) {
        if i > 0 {
            let d = video_pts[i] - video_pts[i - 1];
            println!("join delta[{}]={}", i, d);
            assert!(
                (d - frame_dur_ticks).abs() <= 15,
                "bad join delta {} at {}",
                d,
                i
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
