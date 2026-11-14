// Capability detection for <media-player> tier selection.
// Tier 1: native <video> (H.264/AAC MP4, WebM, HLS via native)
// Tier 2: MSE remux (hls.js / mpegts.js / mux.js)
// Tier 3: WebCodecs + JS/WASM demuxer (mediabunny / web-demuxer)
// Tier 4: "unsupported, use desktop app"

export interface CapabilityResult {
  tier: 1 | 2 | 3 | 4;
  canPlayNative: boolean;
  canPlayMSE: boolean;
  canPlayWebCodecs: boolean;
  reason: string;
}

export function detectCapabilities(): CapabilityResult {
  const video = document.createElement('video');

  // Tier 1: native <video> - check more codec combinations
  const codecChecks = [
    // H.264 / AVC profiles
    'video/mp4; codecs="avc1.42E01E"',
    'video/mp4; codecs="avc1.4D401E"',
    'video/mp4; codecs="avc1.64001E"',
    'video/mp4; codecs="avc1.42E01F"',
    'video/mp4; codecs="avc1.4D401F"',
    // H.265 / HEVC
    'video/mp4; codecs="hev1.1.6.L93.B0"',
    'video/mp4; codecs="hvc1.1.6.L93.B0"',
    // VP8 / VP9
    'video/webm; codecs="vp8"',
    'video/webm; codecs="vp9"',
    'video/webm; codecs="vp09.00.10.08"',
    // AV1
    'video/webm; codecs="av01.0.05M.08"',
    'video/mp4; codecs="av01.0.05M.08"',
    // HLS / DASH
    'application/vnd.apple.mpegurl',
    'application/dash+xml',
    // Audio codecs
    'video/mp4; codecs="mp4a.40.2"', // AAC-LC
    'video/mp4; codecs="mp4a.40.5"', // HE-AAC v1
    'video/mp4; codecs="mp4a.40.29"', // HE-AAC v2
    'video/webm; codecs="opus"',
    'video/webm; codecs="vorbis"',
  ];

  let nativeSupported = false;
  let supportedCodec = '';

  for (const codec of codecChecks) {
    if (video.canPlayType(codec) !== '') {
      nativeSupported = true;
      supportedCodec = codec;
      break;
    }
  }

  if (nativeSupported) {
    return {
      tier: 1,
      canPlayNative: true,
      canPlayMSE: false,
      canPlayWebCodecs: false,
      reason: `Native <video> element supports: ${supportedCodec}`,
    };
  }

  // Tier 2: MSE remux
  const canPlayMSE = 'MediaSource' in window && 'SourceBuffer' in window;
  if (canPlayMSE) {
    return {
      tier: 2,
      canPlayNative: false,
      canPlayMSE: true,
      canPlayWebCodecs: false,
      reason: 'Media Source Extensions available for MSE remux',
    };
  }

  // Tier 3: WebCodecs + JS/WASM demuxer
  // Check both VideoDecoder and AudioDecoder support
  const hasVideoDecoder = 'VideoDecoder' in window && 'VideoFrame' in window;
  const hasAudioDecoder = 'AudioDecoder' in window && 'AudioData' in window;
  const canPlayWebCodecs = hasVideoDecoder && hasAudioDecoder;

  if (canPlayWebCodecs) {
    return {
      tier: 3,
      canPlayNative: false,
      canPlayMSE: false,
      canPlayWebCodecs: true,
      reason: 'WebCodecs API (VideoDecoder + AudioDecoder) available for decode + WASM demuxer',
    };
  }

  // Check for partial WebCodecs support (video only)
  if (hasVideoDecoder) {
    return {
      tier: 3,
      canPlayNative: false,
      canPlayMSE: false,
      canPlayWebCodecs: true,
      reason: 'WebCodecs VideoDecoder available (AudioDecoder not supported, audio will use fallback)',
    };
  }

  // Tier 4: unsupported
  return {
    tier: 4,
    canPlayNative: false,
    canPlayMSE: false,
    canPlayWebCodecs: false,
    reason: 'No supported playback capability detected',
  };
}

export function getSupportedExtensions(): string[] {
  const video = document.createElement('video');
  const exts: string[] = [];

  const checks: [string, string][] = [
    ['mp4', 'video/mp4; codecs="avc1.42E01E"'],
    ['mp4', 'video/mp4; codecs="avc1.4D401E"'],
    ['mp4', 'video/mp4; codecs="hev1.1.6.L93.B0"'],
    ['webm', 'video/webm; codecs="vp8"'],
    ['webm', 'video/webm; codecs="vp9"'],
    ['webm', 'video/webm; codecs="av01.0.05M.08"'],
    ['mp4', 'video/mp4; codecs="av01.0.05M.08"'],
    ['ogg', 'video/ogg; codecs="theora"'],
    ['m3u8', 'application/vnd.apple.mpegurl'],
    ['mpd', 'application/dash+xml'],
  ];

  for (const [ext, type] of checks) {
    if (video.canPlayType(type) !== '') {
      if (!exts.includes(ext)) {
        exts.push(ext);
      }
    }
  }

  if ('MediaSource' in window) {
    exts.push('ts', 'm2ts');
  }

  // WebCodecs can handle more formats via WASM demuxers
  const hasVideoDecoder = 'VideoDecoder' in window && 'VideoFrame' in window;
  const hasAudioDecoder = 'AudioDecoder' in window && 'AudioData' in window;
  if (hasVideoDecoder && hasAudioDecoder) {
    exts.push('mkv', 'mov', 'avi', 'flv', 'webm');
  }

  return exts;
}