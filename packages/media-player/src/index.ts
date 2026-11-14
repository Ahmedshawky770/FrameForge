// Main <media-player> custom element.
// PLAYBACK ONLY - no cut/merge in the browser.
// Tiered strategy: native <video> -> MSE remux -> WebCodecs + JS/WASM demuxer -> unsupported.

import { MediaPlayerBase, PlayerOptions, PlayerEvents } from './tiers';
import { detectCapabilities, CapabilityResult } from './capability-detect';

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
export class MediaPlayerElement extends HTMLElement {
  protected player: MediaPlayerBase | null = null;

  static get observedAttributes() {
    return ['src', 'locale', 'autoplay', 'muted', 'loop', 'playsinline'];
  }

  constructor() {
    super();
  }

  connectedCallback(): void {
    this.init();
  }

  disconnectedCallback(): void {
    if (this.player) {
      this.player.destroy();
      this.player = null;
    }
  }

  attributeChangedCallback(name: string, oldValue: string, newValue: string): void {
    if (oldValue === newValue) return;
    this.reinit();
  }

  private init(): void {
    const options: PlayerOptions = {
      src: this.getAttribute('src') || '',
      locale: (this.getAttribute('locale') as 'en' | 'ar') || 'en',
      autoplay: this.hasAttribute('autoplay'),
      muted: this.hasAttribute('muted'),
      loop: this.hasAttribute('loop'),
      playsInline: this.hasAttribute('playsinline'),
    };

    const events: PlayerEvents = {
      onPlay: () => this.dispatchEvent(new CustomEvent('play', { bubbles: true })),
      onPause: () => this.dispatchEvent(new CustomEvent('pause', { bubbles: true })),
      onSeeked: (time) => this.dispatchEvent(new CustomEvent('seeked', { bubbles: true, detail: { time } })),
      onEnded: () => this.dispatchEvent(new CustomEvent('ended', { bubbles: true })),
      onError: (msg) => this.dispatchEvent(new CustomEvent('error', { bubbles: true, detail: { message: msg } })),
      onLoadedMetadata: (duration) => this.dispatchEvent(new CustomEvent('loadedmetadata', { bubbles: true, detail: { duration } })),
      onTimeUpdate: (time) => this.dispatchEvent(new CustomEvent('timeupdate', { bubbles: true, detail: { time } })),
    };

    this.player = new MediaPlayerBase(this, { ...options, events });
  }

  private reinit(): void {
    if (this.player) {
      this.player.destroy();
    }
    this.init();
  }

  /**
   * Start or resume playback
   * @returns Promise that resolves when playback starts
   */
  async play(): Promise<void> {
    if (!this.player) return;
    return this.player.play();
  }

  /**
   * Pause playback
   */
  async pause(): Promise<void> {
    if (!this.player) return;
    this.player.pause();
  }

  /**
   * Seek to a specific time
   * @param seconds - Time in seconds to seek to
   */
  async seek(seconds: number): Promise<void> {
    if (!this.player) return;
    this.player.seek(seconds);
  }

  /**
   * Set playback rate
   * @param rate - Playback rate (1.0 = normal)
   */
  async setPlaybackRate(rate: number): Promise<void> {
    if (!this.player) return;
    this.player.setPlaybackRate(rate);
  }

  /**
   * Set volume level
   * @param volume - Volume level (0.0 to 1.0)
   */
  async setVolume(volume: number): Promise<void> {
    if (!this.player) return;
    this.player.setVolume(volume);
  }

  /**
   * Set muted state
   * @param muted - Whether to mute
   */
  async setMuted(muted: boolean): Promise<void> {
    if (!this.player) return;
    this.player.setMuted(muted);
  }

  /**
   * Request fullscreen mode
   * @returns Promise that resolves when fullscreen is entered
   */
  async requestFullscreen(): Promise<void> {
    const video = this.videoElement;
    if (video) {
      return video.requestFullscreen();
    }
  }

  /**
   * Exit fullscreen mode
   * @returns Promise that resolves when fullscreen is exited
   */
  async exitFullscreen(): Promise<void> {
    if (document.exitFullscreen) {
      return document.exitFullscreen();
    }
  }

  /**
   * Step forward one frame (not available in browser tier)
   */
  async stepFrame(): Promise<void> {
    console.warn('Frame step not available in web component tier');
  }

  /**
   * Get available tracks
   * @returns Array of track objects (empty in browser tier)
   */
  async getTracks(): Promise<any[]> {
    return [];
  }

  /**
   * Select a track by kind and index (not available in browser tier)
   * @param kind - Track kind (e.g., 'audio', 'subtitle')
   * @param index - Track index to select
   */
  async selectTrack(kind: string, index: number): Promise<void> {
    console.warn('Track selection not available in web component tier');
  }

  /** Total duration of the media in seconds */
  get duration(): number {
    return this.player ? this.player.getDuration() : 0;
  }

  /** Current playback time in seconds */
  get currentTime(): number {
    return this.player ? this.player.getCurrentTime() : 0;
  }

  /** Whether playback is paused */
  get paused(): boolean {
    return this.player ? this.player.getPaused() : true;
  }

  /** Video readyState (0 = HAVE_NOTHING, 4 = HAVE_ENOUGH_DATA) */
  get readyState(): number {
    return this.videoElement ? this.videoElement.readyState : 0;
  }

  /** Whether lossless cutting is supported (always false for web component) */
  get supportsLosslessCut(): boolean {
    return false;
  }

  /** Current locale ('en' | 'ar') */
  get locale(): string {
    return this.getAttribute('locale') || 'en';
  }

  /**
   * Set the locale
   * @param locale - Locale string ('en' | 'ar')
   */
  setLocale(locale: string): void {
    this.setAttribute('locale', locale);
  }

  /** @internal */
  addEventListener(type: string, cb: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions): void {
    super.addEventListener(type, cb, options);
  }

  /** @internal */
  removeEventListener(type: string, cb: EventListenerOrEventListenerObject, options?: boolean | EventListenerOptions): void {
    super.removeEventListener(type, cb, options);
  }

  /**
   * Get the underlying HTMLVideoElement
   * @returns HTMLVideoElement or null if not initialized
   */
  get videoElement(): HTMLVideoElement | null {
    return this.player?.['video'] as HTMLVideoElement | null;
  }

  /** Current playback tier (1 = native, 2 = MSE, 3 = WebCodecs, 4 = unsupported) */
  get tier(): number {
    return this.player ? this.player.getTier() : 4;
  }

  /** Browser capability detection result */
  static get capabilities(): CapabilityResult {
    return detectCapabilities();
  }
}

// Register the custom element
if (typeof customElements !== 'undefined' && !customElements.get('media-player')) {
  customElements.define('media-player', MediaPlayerElement);
}

// Export for module consumers
export { detectCapabilities };
export type { CapabilityResult, PlayerOptions, PlayerEvents };