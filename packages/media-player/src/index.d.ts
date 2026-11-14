// TypeScript declarations for <media-player> custom element

import type { CapabilityResult, PlayerOptions, PlayerEvents } from './tiers';

/**
 * MediaPlayerElement - Web Component for video playback
 * 
 * @element media-player
 * @fires play - Dispatched when playback starts
 * @fires pause - Dispatched when playback pauses
 * @fires seeked - Dispatched when seeking completes, detail: { time: number }
 * @fires ended - Dispatched when playback reaches end
 * @fires error - Dispatched on playback error, detail: { message: string }
 * @fires loadedmetadata - Dispatched when metadata loads, detail: { duration: number }
 * @fires timeupdate - Dispatched on time update, detail: { time: number }
 * @attr {string} src - Video source URL
 * @attr {string} locale - UI locale ('en' | 'ar'), default 'en'
 * @attr {boolean} autoplay - Auto-start playback
 * @attr {boolean} muted - Start muted
 * @attr {boolean} loop - Loop playback
 * @attr {boolean} playsinline - Play inline on mobile
 */
export declare class MediaPlayerElement extends HTMLElement {
  /**
   * Start or resume playback
   * @returns Promise that resolves when playback starts
   */
  play(): Promise<void>;

  /**
   * Pause playback
   */
  pause(): Promise<void>;

  /**
   * Seek to a specific time
   * @param seconds - Time in seconds to seek to
   */
  seek(seconds: number): Promise<void>;

  /**
   * Set playback rate
   * @param rate - Playback rate (1.0 = normal)
   */
  setPlaybackRate(rate: number): Promise<void>;

  /**
   * Set volume level
   * @param volume - Volume level (0.0 to 1.0)
   */
  setVolume(volume: number): Promise<void>;

  /**
   * Set muted state
   * @param muted - Whether to mute
   */
  setMuted(muted: boolean): Promise<void>;

  /**
   * Request fullscreen mode
   * @returns Promise that resolves when fullscreen is entered
   */
  requestFullscreen(): Promise<void>;

  /**
   * Exit fullscreen mode
   * @returns Promise that resolves when fullscreen is exited
   */
  exitFullscreen(): Promise<void>;

  /**
   * Step forward one frame (not available in browser tier)
   */
  stepFrame(): Promise<void>;

  /**
   * Get available tracks
   * @returns Array of track objects (empty in browser tier)
   */
  getTracks(): Promise<any[]>;

  /**
   * Select a track by kind and index (not available in browser tier)
   * @param kind - Track kind (e.g., 'audio', 'subtitle')
   * @param index - Track index to select
   */
  selectTrack(kind: string, index: number): Promise<void>;

  /** Total duration of the media in seconds */
  readonly duration: number;

  /** Current playback time in seconds */
  readonly currentTime: number;

  /** Whether playback is paused */
  readonly paused: boolean;

  /** Video readyState (0 = HAVE_NOTHING, 4 = HAVE_ENOUGH_DATA) */
  readonly readyState: number;

  /** Whether lossless cutting is supported (always false for web component) */
  readonly supportsLosslessCut: boolean;

  /** Current locale ('en' | 'ar') */
  readonly locale: string;

  /**
   * Set the locale
   * @param locale - Locale string ('en' | 'ar')
   */
  setLocale(locale: string): void;

  /** Get the underlying HTMLVideoElement */
  readonly videoElement: HTMLVideoElement | null;

  /** Current playback tier (1 = native, 2 = MSE, 3 = WebCodecs, 4 = unsupported) */
  readonly tier: number;

  /** Browser capability detection result */
  static readonly capabilities: CapabilityResult;
}

/** Global custom elements registry augmentation */
declare global {
  interface HTMLElementTagNameMap {
    'media-player': MediaPlayerElement;
  }
}

export { detectCapabilities } from './capability-detect';
export type { CapabilityResult, PlayerOptions, PlayerEvents } from './tiers';
export { MediaPlayerElement };