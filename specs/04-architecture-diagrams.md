# Architecture Diagrams (ASCII)

Every arrow crossing a process boundary is a memory copy and is labeled "low-copy". Zero-copy is NOT claimed across the WebView process boundary.

## Desktop path (Tauri 2 app)

```
+-----------------------------------------------------------------------+
|                      OS: Linux (Pop!_OS 22.04+)                       |
|  Session: GNOME/COSMIC  |  Display: X11 or Wayland  |  GPU: N/AMD/Intel |
+-----------------------------------------------------------------------+

   [ Tauri Core Process (Rust) ]                 [ WebView Process ]
   [ main thread / IPC host ]  low-copy  <-----> [ WebKitGTK (JS + CSS) ]
   |                               custom URI    |  (separate process)
   |                               protocol      |
   |                                           +---------------------+
   |                                           | <media-player> UI   |
   |                                           | (vanilla JS + Vite) |
   |                                           +---------------------+
   |                                                     |
   |  (Tauri commands)   serde JSON over IPC             | DOM events
   |  <------------------->                            (pointer/key)
   |                                                     v
   |  [ tauri-plugin-player ]                         [ UI layer ]
   |  - command dispatcher                            - transport icons
   |  - event emitter                                 - timeline
   |  - state (player, EDL, export)                   - Arabic/RTL
   |                                                     (isolates)
   |                                                     |
   |                                                     v
   |                                        [ data: base64 RGBA frames ]
   |                                        low-copy: Tauri command return
   |                                                     |
   |                                                     v
   |                                        [ <canvas> scrub preview ]
   |                                 (raw RGBA via data URL; benchmark decides)
   |
   |  [ mpv-adapter ]  libmpv (dynamic)  [ libmpv.so ]
   |  - play/pause/seek/speed/volume       (render API or separate window)
   |  - audio/video sync -> PipeWire/Pulse
   |  - hwdec: VAAPI / NVDEC (frames on GPU)
   |  - libass subtitles (Arabic shaping + RTL)
   |  - MPRIS D-Bus (org.mpris.MediaPlayer2)
   |                                                     [ X11 child window ]
   |                                                     [ or Wayland native ]
   |                                                     (video surface)
   |
   |  [ media-probe ]    [ scrub-engine ]    [ edit-engine ]   [ export-engine ]
   |  ffmpeg-next        keyframe index   cut/merge planning  stream-copy +
   |  (libavformat)      seek-and-decode  EDL validation      re-encode pipeline
   |                                                   (libavcodec/swscale/
   |                                                   swresample/filter)
   |                                                   VAAPI/NVENC encode
   |                                                   atomic write (temp+rename)
   |
   |  [ hw-diagnostics ]   [ cli ] (headless)
   |  detect VAAPI/NVENC   export, probe, bench
   +-----------------------------------------------------------------------+

Copy points (desktop):
1. libmpv -> GPU texture (hwdec, zero copy on GPU path during playback)
2. GPU -> CPU download (scrub preview only; low-copy)
3. CPU -> WebView via Tauri command return (low-copy; base64 RGBA)
4. File -> FFmpeg demuxer (in-process, no process boundary)
5. FFmpeg encoder -> temp file on disk (atomic rename on commit)

Process boundary: Tauri host <-> WebView (only copy 3). All other copies are in-process.
```

## Web path (`<media-player>` component)

```
+-----------------------------------------------------------------------+
|                      Browser (WebKitGTK or other)                     |
|  [ <media-player> custom element ]  shadow DOM                        |
|  - capability detection                                           |
|  Tier 1: native <video>  (H.264/AAC MP4, WebM, HLS via native)      |
|  Tier 2: MSE remux         (hls.js / mpegts.js / mux.js)            |
|              SourceBuffer appendBuffer (copy within browser)        |
|  Tier 3: WebCodecs + JS/WASM demuxer (mediabunny / web-demuxer)     |
|              videoFrame -> GPU (no CPU pixel touch on decode path)  |
|  Tier 4: "unsupported, use desktop app" message                     |
+-----------------------------------------------------------------------+
                             |
                             |  NO native Rust/libmpv call possible
                             |  from a normal web page (sandbox)
                             v
                    [ FrameForge desktop app ]  (native, separate process)

Copy points (web):
1. Network -> browser cache (in-browser)
2. MSE: bytes -> SourceBuffer (copy within browser process)
3. WebCodecs: encoded -> VideoFrame (GPU-backed; low-copy if GPU upload stays on GPU)
4. Scrub preview (desktop-only custom protocol): not applicable to web tier

Hard constraint: No SharedArrayBuffer unless COOP/COEP verified on WebKitGTK.
```

## Memory-copy labeling key
- `low-copy` = one bounded copy across a process boundary; documented; sized.
- `zero-copy` = NOT used across WebView boundary (prohibited by hard constraint).
- In-process copies (demux, encode) are not labeled as they do not cross a boundary.