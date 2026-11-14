# Desktop Environment Matrix Testing

This document defines the test matrix for FrameForge across desktop environments,
display servers, and GPU vendors. Tests are run on available hardware; untested
combinations are marked as "Experimental".

## Test Matrix

| # | Session | Display | GPU Vendor | GPU Driver | HW Decode | HW Encode | Video Presents | Input/Overlay | Status |
|---|---------|---------|------------|------------|-----------|-----------|----------------|---------------|--------|
| 1 | GNOME | X11 | Intel | i965/VAAPI | VAAPI | VAAPI | ? | ? | Experimental |
| 2 | GNOME | Wayland | Intel | i965/VAAPI | VAAPI | VAAPI | ? | ? | Experimental |
| 3 | GNOME | X11 | AMD | Radeon/VAAPI | VAAPI | VAAPI | ? | ? | Experimental |
| 4 | GNOME | Wayland | AMD | Radeon/VAAPI | VAAPI | VAAPI | ? | ? | Experimental |
| 5 | GNOME | X11 | NVIDIA | NVDEC | NVDEC | NVENC | ? | ? | Experimental |
| 6 | GNOME | Wayland | NVIDIA | NVDEC | NVDEC | NVENC | ? | ? | Experimental |
| 7 | COSMIC | X11 | Intel | VAAPI | VAAPI | VAAPI | ? | ? | Experimental |
| 8 | COSMIC | Wayland | Intel | VAAPI | VAAPI | VAAPI | ? | ? | Experimental |
| 9 | COSMIC | X11 | AMD | VAAPI | VAAPI | VAAPI | ? | ? | Experimental |
| 10 | COSMIC | Wayland | AMD | VAAPI | VAAPI | VAAPI | ? | ? | Experimental |
| 11 | COSMIC | X11 | NVIDIA | NVDEC | NVDEC | NVENC | ? | ? | Experimental |
| 12 | COSMIC | Wayland | NVIDIA | NVDEC | NVDEC | NVENC | ? | ? | Experimental |

## Test Procedure

For each matrix entry:

### Prerequisites
1. Install the target desktop session (GNOME or COSMIC)
2. Configure the display server (X11 or Wayland)
3. Install the appropriate GPU driver
4. Verify hardware acceleration:
   ```bash
   vainfo                          # VAAPI decode/encode
   nvidia-smi                      # NVIDIA GPU status
   intel_gpu_top                   # Intel GPU usage
   radeontop                       # AMD GPU usage
   ```

### Video Playback Test
1. Launch FrameForge: `frameforge`
2. Load a test file: `test-media/test-video.mp4` (1080p H.264)
3. Verify:
   - Video displays without black frames
   - Audio plays with correct A/V sync
   - Hardware decode is active (check via `intel_gpu_top` / `radeontop` / `nvidia-smi`)

### Precision Scrubbing Test
1. Drag the timeline scrubber
2. Verify frame-accurate preview while dragging
3. Verify exact full-res frame on release
4. Measure input-to-frame latency (target: p95 < 100 ms)

### Cut/Merge Test
1. Select segments using the timeline
2. Export using stream-copy (keyframe-aligned)
3. Export using re-encode (non-keyframe cuts)
4. Verify output plays correctly with A/V sync

### Web Component Test
1. Open `<media-player>` in a browser
2. Verify native `<video>` playback for MP4
3. Verify tier fallback for unsupported formats

## Known Issues

| # | Issue | Affected Matrix Entries | Workaround |
|---|-------|------------------------|------------|
| 1 | WebKitGTK transparency on Wayland | 2, 4, 6, 8, 10, 12 | Use X11 or Strategy B (separate window) |
| 2 | NVIDIA + Wayland + GBM/EGL | 6, 12 | Use X11 for NVIDIA in v1 |
| 3 | VAAPI not installed by default | 1-4, 7-10 | Install `libva-drm2` / `mesa-va-drivers` |
| 4 | NVENC not available on consumer NVIDIA | 5, 6, 11, 12 | Use software x264 fallback |
| 5 | X11 child window resize race | All X11 entries | Frontend calls `player_resize_child_window` on resize |

## Test Files

| File | Codec | Resolution | Frame Rate | Purpose |
|------|-------|------------|------------|---------|
| test-media/test-video.mp4 | H.264 | 1920x1080 | 60 fps | General playback |
| test-media/all-intra.mp4 | H.264 (all-intra) | 1920x1080 | 30 fps | Keyframe snapping |
| test-media/long-gop.ts | MPEG-2 | 1920x1080 | 25 fps | Partial-index fallback |
| test-media/4k-hevc.mkv | HEVC | 3840x2160 | 30 fps | 4K HW decode |
| test-media/vfr.mkv | H.264 (VFR) | 1280x720 | VFR | PTS-based seeking |

## Success Criteria

- Video displays on all tested combinations
- Playback maintains ≥95% frames at 1080p30
- Scrub p95 latency < 100 ms on 1080p
- Stream-copy export produces bit-identical segments
- Re-encode export maintains A/V sync within 1 frame
- MPRIS controls work from system media indicator
- Arabic RTL renders correctly on all tested DEs