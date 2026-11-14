# Key Algorithms (Rust skeleton / pseudocode)

All times use rational timebases internally; no float PTS. No microsecond-precision claims — accuracy bounded by frame duration.

## 8.1 Keyframe indexing (progressive background)
Pseudocode:
```
build_keyframe_index(path):
    ctx = avformat_open_input(path)
    avformat_find_stream_info(ctx)
    video_stream = find video stream
    index = empty list
    pkt = read frames sequentially (background thread)
    for each pkt on video stream:
        if pkt.flags & AV_PKT_FLAG_KEY:
            index.push((pkt.pts, pkt.pos))  // pts -> file offset
        if time_budget_exceeded: break   // progressive: index grows over time
    return index
```
- MPEG-TS has no index; partial-index fallback: index what's read, fall back to av_seek_frame + decode-forward for unindexed regions.
- Store index in memory (LRU-capped); persist optional sidecar `.ffindex`.
- Implemented in `frameforge_media_probe::build_keyframe_index` using ffmpeg-next packet iteration.

## 8.2 Seek-and-decode (frame-accurate preview)
```
seek_and_decode(target_pts):
    kf = latest keyframe with kf.pts <= target_pts
    av_seek_frame(video_stream, kf.pts, AVSEEK_FLAG_BACKWARD)
    loop:
        pkt = av_read_frame()
        decode pkt -> frame
        if frame.pts >= target_pts: return frame   // discard-before-target
        if EOF: return last frame or error
    // B-frame reordering: DTS may not be monotonic; compare on PTS, buffer reorder queue
    // VFR: use pts, not frame index; target is a PTS not a frame number
    // Open-GOP edge: frames before first keyframe in GOP may reference out-of-GOP; clamp to keyframe start
```
Latest-wins cancellation: if a newer seek arrives while decoding, abort current decode and restart from new target (atomic flag swap).

Implemented in `frameforge_scrub_engine::ScrubSession::seek`:
- Converts target to stream timebase
- `ictx.seek(target_pts, i64::MIN..target_pts)` to keyframe at-or-before
- Decodes forward, discarding frames before target
- Returns first frame at/after target as RGBA via swscale

## 8.3 Latest-wins scrubbing
```
on_drag(target_pts):
    cancel_token = new token
    current_token = cancel_token.clone()
    spawn task:
        decode_frame_at(target_pts, current_token)
        if !current_token.cancelled:
            deliver_preview(frame)   // latest-wins: only most recent target renders
on_drag_end(target_pts):
    decode_frame_at(target_pts, fresh_token)  // exact full-res frame on release
    deliver_full_res(frame)
```
- Input throttled to rAF/vsync, not "microsecond ticks".
- Low-res preview while dragging (decode at reduced scale or cached low-res); exact full-res on release.
- GOP/frame LRU cache: keep recently decoded keyframe neighborhoods to avoid re-decoding on small drags.

Latency budget (targets, to be validated): p50 < 50 ms, p95 < 100 ms for 1080p; 4K may exceed.

Implemented in `frameforge_scrub_engine::ScrubSession`:
- `seek_to_file()` opens/reuses file, seeks, decodes
- `next_frame()` continues from current decode position
- `prev_frame()` seeks back ~1 frame duration, decodes forward to find previous
- `CancelToken` shared between UI and decode task for latest-wins cancellation

## 8.4 EDL -> export plan
```
plan_export(edl):
    for each clip: probe stream params
    plan.segments = []
    for each track in edl.tracks (enabled only):
        for each item in track.items (enabled only):
            plan.order.push(item.source_id)
            snap_mode = if export.mode == StreamCopy { Keyframe } else { Exact }
            cut_points = snap_cut_points(item.path, item.source_range, snap_mode)
            plan.segments.push(PlannedSegment{source_id, item_id, range, cut_points, timeline_start, track_index, ...})
    plan.merge_plan = {
        order: edl.clips order,
        lossless: all_compatible && all_keyframe_aligned,
        reasons: compatibility_reasons,
        alternative: if lossless { stream-copy } else { re-encode with normalization }
    }
    return plan
```

Implemented in `frameforge_edit_engine::plan_export`:
- Iterates enabled tracks and items
- Converts frontend segments (f64 seconds) to microsecond `Timestamp` via `IMPORT_TIMEBASE = Timebase(1, 1_000_000)`
- `snap_cut_points()` uses `build_keyframe_index()` for Keyframe mode, exact timestamps for Exact mode
- `analyze_compatibility()` probes each clip's first video stream and compares codec/profile/res/fps/timebase/pixfmt/audio

## 8.5 Compatibility analyzer
```
analyze_compatibility(clips):
    ref = clips[0].streams
    reasons = []
    for c in clips[1..]:
        for each stream kind (video/audio):
            if c.codec != ref.codec: reasons.push("codec mismatch: ...")
            if c.profile != ref.profile: reasons.push("profile mismatch")
            if c.resolution != ref.resolution: reasons.push("resolution mismatch")
            if c.fps != ref.fps: reasons.push("frame rate mismatch")
            if c.timebase != ref.timebase: reasons.push("timebase mismatch")
            if c.pix_fmt != ref.pix_fmt: reasons.push("pixel format mismatch")
            if c.audio.sample_rate != ref.audio.sample_rate: reasons.push("audio sample rate mismatch")
            if c.audio.channels != ref.audio.channels: reasons.push("audio channel mismatch")
    lossless_possible = reasons.is_empty() && cuts_keyframe_aligned
    alternative = if lossless_possible { stream-copy } else { re-encode with normalization }
    return CompatibilityReport{lossless_possible, reasons, alternative}
```
Documented impossibles: different codec/profile/res/fps/timebase/pixfmt/audio; non-keyframe cut in copy mode; VFR vs CFR mismatch; B-frame/open-GOP joins causing drift.

Implemented in `frameforge_edit_engine::analyze_compatibility_tracks`:
- Probes each clip's first video stream via `frameforge_media_probe::probe()`
- `StreamParams::compatible_with()` compares all relevant parameters
- Returns `CompatibilityReport` with human-readable reasons

## 8.6 Multi-segment stream-copy merge with timestamp rebasing
```
stream_copy_merge(segments):
    out_ctx = create output container (matching codec)
    for each stream: copy codecpar, create out_stream
    base_offset = 0  // per stream
    last_dts[stream] = None
    for seg in segments:
        in_ctx = open seg.source
        seek to seg.range.start (keyframe)
        for pkt in read until seg.range.end:
            // timestamp rebasing
            pkt.pts  -= seg_start_pts; pkt.dts -= seg_start_dts
            pkt.pts  += base_offset[stream]; pkt.dts += base_offset[stream]
            // discontinuity: set AV_PKT_FLAG_DISCONTINUITY at segment start
            pkt.flags |= AV_PKT_FLAG_DISCONTINUITY  (first pkt of segment)
            // enforce DTS monotonicity per stream
            if last_dts[stream] is Some and pkt.dts < last_dts[stream]:
                pkt.dts = last_dts[stream] + 1   // clamp, log drift
            last_dts[stream] = pkt.dts
            av_interleaved_write_frame(out_ctx, pkt)
        base_offset[stream] += duration_of_written(seg)
    // edit-list / negative-timestamp handling: MP4 uses edit lists; MKV/TS use block duration; clamp negatives to 0
    av_write_trailer(out_ctx)
```
- Packet-by-packet; never loads files into RAM.
- Audio/video drift at joins: clamp DTS, warn if drift exceeds threshold.
- Container/codec compat: MP4 cannot hold some codecs (e.g., certain subtitle codecs); MKV/TS more permissive; analyzer reports.

Implemented in `frameforge_export_engine::remux_multiple_files` → `remux_segment`:
- Groups segments by source file to minimize open/close
- Per-stream segment start PTS captured for independent audio/video alignment
- `RemuxCtx` holds base_offsets, last_dts, stream_seg_start per stream
- DTS monotonicity enforced with clamping
- Output format auto-detected from extension (mp4, mkv, ts, mov, webm, avi, etc.)

## 8.7 Re-encode fallback
```
reencode_export(plan):
    for seg in plan.segments:
        decoder = open seg.source (hwdec if available, frames on GPU)
        for frame in decode range:
            // GPU->CPU download only if software encode or filter needed
            frame = swscale(frame, target.width, target.height, target.pix_fmt)
            frame = filter(frame, fps normalize if VFR->CFR)
            encoded = av_encode(frame, vcodec=VAAPI/NVENC or x264/x265/SVT-AV1)
            write encoded to temp file
        audio = swresample(audio, target.sample_rate, target.channels)
        encoded_audio = aac/opus encode
    mux to temp file, then atomic rename to destination
```
- HW encode: VAAPI/NVENC; software fallback x264/x265/SVT-AV1 (licensing allows).
- Progress + cancellation supported; resumability not required.
- Quality presets map to x264/x265 preset + CRF: high=veryslow/18, medium=medium/23, low=fast/28.

Implemented in `frameforge_export_engine::reencode_multiple_files`:
- Creates decoder/encoder per stream from first clip
- `VideoTranscoder`/`AudioTranscoder` structs hold decoder+encoder
- `normalize_video_frame()` via swscale for resolution/pix_fmt conversion
- Per-segment PTS offset (`seg_start_output_pts`) prevents PTS accumulation
- Encoder flush at end with base_offset applied
- Atomic write via `atomic_write()` (temp file + rename)

## 8.8 Locale / RTL switching
```
set_locale(locale):   // 'en' | 'ar'
    i18n.locale = locale
    document.dir = locale == 'ar' ? 'rtl' : 'ltr'
    re-render all Fluent strings
    // mirroring table:
    // mirrors in RTL: timeline direction (playhead moves right-to-left), transport icon order (prev/next swap), start/end alignment
    // does NOT mirror: timecode digits, video content, transport play/pause/stop icons themselves (symmetrical), numbers unless Arabic-Indic option on
    // UAX #9: wrap mixed Arabic/Latin filenames in PDI/PDL isolates
    // numerals: Western vs Arabic-Indic user option
    // fonts: Arabic font stack + line-height adjustment
```
- Runtime switchable without reload.
- Persist choice to settings; detect system locale on first launch.
- Implemented in `tauri-plugin-player` → `locale_theme` commands + frontend CSS `dir` attribute.

## Assumptions
- ffmpeg-next API matches system FFmpeg 4.4.2 (unverified — build test needed). (Assumption)
- libmpv 0.34.x API stable (unverified — installed version TBD). (Assumption)
- Pseudocode uses Rust-like syntax; not a complete implementation.