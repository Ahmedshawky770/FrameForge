# @frameforge/media-player

Playback-only web component `<media-player>` for websites. Part of the FrameForge video editing ecosystem.

## Tiered Playback Strategy

The component uses a tiered capability detection strategy:

1. **Native `<video>`** — H.264/AAC MP4, WebM, native HLS/DASH
2. **MSE remux** — hls.js, mpegts.js, mux.js for HLS/MPEG-TS/fMP4
3. **WebCodecs + JS/WASM demuxer** — mediabunny/web-demuxer for MKV etc.
4. **Unsupported** — Clear message to use the desktop app

## Installation

```bash
npm install @frameforge/media-player
```

## Usage

```html
<script type="module">
  import '@frameforge/media-player';
</script>

<media-player src="clip.mp4" locale="en" autoplay controls></media-player>
```

## JavaScript API

```js
const player = document.querySelector('media-player');
await player.play();
await player.seek(10.5);
await player.setPlaybackRate(1.5);
await player.setVolume(0.5);
await player.setMuted(false);

player.addEventListener('play', () => console.log('playing'));
player.addEventListener('pause', () => console.log('paused'));
player.addEventListener('seeked', (e) => console.log('seeked to', e.detail.time));
player.addEventListener('ended', () => console.log('ended'));
player.addEventListener('error', (e) => console.log('error', e.detail.message));
player.addEventListener('loadedmetadata', (e) => console.log('duration', e.detail.duration));
player.addEventListener('timeupdate', (e) => console.log('time', e.detail.time));
```

## Properties

| Property | Type | Description |
|----------|------|-------------|
| `src` | string | Media file URL |
| `locale` | 'en' \| 'ar' | UI language (for error messages) |
| `autoplay` | boolean | Auto-play on load |
| `muted` | boolean | Start muted |
| `loop` | boolean | Loop playback |
| `playsinline` | boolean | Play inline on mobile |

## Events

| Event | Detail | Description |
|-------|--------|-------------|
| `play` | — | Playback started |
| `pause` | — | Playback paused |
| `seeked` | `{ time }` | Seek completed |
| `ended` | — | Playback finished |
| `error` | `{ message }` | Playback error |
| `loadedmetadata` | `{ duration }` | Metadata loaded |
| `timeupdate` | `{ time }` | Time updated |

## Methods

| Method | Returns | Description |
|--------|---------|-------------|
| `play()` | `Promise<void>` | Start/resume playback |
| `pause()` | `Promise<void>` | Pause playback |
| `seek(seconds)` | `Promise<void>` | Seek to time |
| `setPlaybackRate(rate)` | `Promise<void>` | Set speed (1.0 = normal) |
| `setVolume(volume)` | `Promise<void>` | Set volume (0.0-1.0) |
| `setMuted(muted)` | `Promise<void>` | Set muted state |
| `requestFullscreen()` | `Promise<void>` | Enter fullscreen |
| `exitFullscreen()` | `Promise<void>` | Exit fullscreen |
| `stepFrame()` | `Promise<void>` | Frame step (N/A in browser) |
| `getTracks()` | `Promise<any[]>` | Get tracks (empty in browser) |
| `selectTrack(kind, index)` | `Promise<void>` | Select track (N/A in browser) |

## Read-only Properties

| Property | Type | Description |
|----------|------|-------------|
| `duration` | number | Total duration in seconds |
| `currentTime` | number | Current playback time in seconds |
| `paused` | boolean | Whether playback is paused |
| `readyState` | number | HTMLMediaElement readyState (0-4) |
| `supportsLosslessCut` | boolean | Always `false` for web component |
| `locale` | string | Current locale ('en' or 'ar') |
| `videoElement` | HTMLVideoElement \| null | Underlying video element |
| `tier` | number | Current playback tier (1-4) |

## Static Properties

| Property | Type | Description |
|----------|------|-------------|
| `capabilities` | `CapabilityResult` | Browser capability detection result |

## Capability Detection

```ts
import { detectCapabilities, CapabilityResult } from '@frameforge/media-player';

const caps: CapabilityResult = detectCapabilities();
// caps.tier: 1 | 2 | 3 | 4
// caps.canPlayNative: boolean
// caps.canPlayMSE: boolean
// caps.canPlayWebCodecs: boolean
// caps.reason: string

const exts = getSupportedExtensions(); // string[]
```

## Tier Details

### Tier 1: Native `<video>`
- Direct `<video src="...">` playback
- Supports: H.264/AAC MP4, WebM (VP8/VP9/AV1), native HLS/DASH
- Zero dependencies, best performance

### Tier 2: MSE Remux
- Loads hls.js / mpegts.js / mux.js from CDN (jsdelivr)
- Transmuxes HLS/MPEG-TS/fMP4 to ISOBMFF fragments
- Appends to MediaSource SourceBuffer
- Requires: `MediaSource` + `SourceBuffer` APIs

### Tier 3: WebCodecs + WASM Demuxer
- Uses `VideoDecoder` + `AudioDecoder` + `VideoFrame` + `AudioData`
- Requires WASM demuxer (mediabunny/web-demuxer) — not bundled
- Shows info message with desktop app download link
- Partial support if only `VideoDecoder` available

### Tier 4: Unsupported
- Shows formatted message with "Download Desktop App" button
- Button links to `https://frameforge.app/download`

## Internationalization

- `locale="en"` (default) or `locale="ar"`
- RTL layout applied automatically for Arabic
- Error messages localized
- Uses `dir="rtl"` / `dir="ltr"` on component root

## Limitations

- **Playback only** — No cut/merge in the browser (v1 limitation)
- **No native libmpv** — A normal web page cannot call native Rust/libmpv due to browser sandbox
- **No SharedArrayBuffer** — Unless COOP/COEP is verified on WebKitGTK
- **Format support** — Depends on browser codec support and available libraries
- **No frame step** — `stepFrame()` logs warning
- **No track selection** — `getTracks()`/`selectTrack()` log warnings

## Browser Support

| Feature | Chrome | Firefox | Safari | Edge |
|---------|--------|---------|--------|------|
| Native `<video>` | ✅ | ✅ | ✅ | ✅ |
| MSE | ✅ | ✅ | ✅ | ✅ |
| WebCodecs | ✅ (94+) | ✅ (113+) | ✅ (16.4+) | ✅ (94+) |

## Build

```bash
cd packages/media-player
npm install
npm run build  # outputs to dist/ (ESM + CJS + .d.ts)
```

## License

GPL-3.0-or-later. See LICENSE for details.