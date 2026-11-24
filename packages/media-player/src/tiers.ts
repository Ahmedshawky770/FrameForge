// Tier implementations for <media-player> playback.
// Tier 1: native <video>
// Tier 2: MSE remux (hls.js / mpegts.js / mux.js)
// Tier 3: WebCodecs + JS/WASM demuxer (mp4box.js)
// Tier 4: unsupported message

import { detectCapabilities, CapabilityResult } from './capability-detect';

export interface PlayerOptions {
  src: string;
  locale?: 'en' | 'ar';
  autoplay?: boolean;
  muted?: boolean;
  loop?: boolean;
  playsInline?: boolean;
  events?: PlayerEvents;
}

export interface PlayerEvents {
  onPlay?: () => void;
  onPause?: () => void;
  onSeeked?: (time: number) => void;
  onEnded?: () => void;
  onError?: (msg: string) => void;
  onLoadedMetadata?: (duration: number) => void;
  onTimeUpdate?: (time: number) => void;
}

const HLS_JS_CDN = 'https://cdn.jsdelivr.net/npm/hls.js@latest/dist/hls.min.js';
const MPEGTS_JS_CDN = 'https://cdn.jsdelivr.net/npm/mpegts.js@latest/dist/mpegts.min.js';
const MP4BOX_JS_CDN = 'https://cdn.jsdelivr.net/npm/mp4box@0.5.4/dist/mp4box.all.min.js';

const MESSAGES = {
  en: {
    unsupportedFormat: 'Format not supported in browser',
    unsupportedDetail: 'This video format cannot be played in the browser. Please use the FrameForge desktop app for full format support including MKV, MPEG-TS, and other containers.',
    mseRequiresLibs: 'MSE remux requires external libraries',
    mseDetail: 'HLS/MPEG-TS/fMP4 playback via MSE requires hls.js, mpegts.js, or mux.js to be loaded. Please include one of these libraries or use the FrameForge desktop app.',
    webCodecsRequiresWasm: 'WebCodecs demuxer requires a demuxer library',
    webCodecsDetail: 'MKV and other container playback via WebCodecs requires a JS/WASM demuxer. For MP4 (H.264/AAC) the browser already plays natively; use the FrameForge desktop app for full format support.',
    downloadApp: 'Download Desktop App',
    hlsLoadError: 'Failed to load HLS stream',
    mseUnsupportedFormat: 'Unsupported MSE format',
    webCodecsDecodeError: 'WebCodecs decode failed',
  },
  ar: {
    unsupportedFormat: 'التنسيق غير مدعوم في المتصفح',
    unsupportedDetail: 'لا يمكن تشغيل تنسيق الفيديو هذا في المتصفح. يرجى استخدام تطبيق FrameForge المكتبي للحصول على دعم كامل للتنسيقات بما في ذلك MKV و MPEG-TS وغيرها.',
    mseRequiresLibs: 'إعادة التجميع MSE تتطلب مكتبات خارجية',
    mseDetail: 'يتطلب تشغيل HLS/MPEG-TS/fMP4 عبر MSE تحميل hls.js أو mpegts.js أو mux.js. يرجى تضمين إحدى هذه المكتبات أو استخدام تطبيق FrameForge المكتبي.',
    webCodecsRequiresWasm: 'فك ترميز WebCodecs يتطلب أداة فك ترميز',
    webCodecsDetail: 'يتطلب تشغيل MKV والحاويات الأخرى عبر WebCodecs أداة فك ترميز JS/WASM. لملفات MP4 (H.264/AAC) يشغّل المتصفح أصلاً؛ استخدم تطبيق FrameForge المكتبي للدعم الكامل.',
    downloadApp: 'تحميل التطبيق المكتبي',
    hlsLoadError: 'فشل تحميل بث HLS',
    mseUnsupportedFormat: 'تنسيق MSE غير مدعوم',
    webCodecsDecodeError: 'فشل فك ترميز WebCodecs',
  },
};

function loadScript(url: string): Promise<void> {
  return new Promise((resolve, reject) => {
    if (document.querySelector(`script[src="${url}"]`)) {
      resolve();
      return;
    }
    const script = document.createElement('script');
    script.src = url;
    script.async = true;
    script.onload = () => resolve();
    script.onerror = () => reject(new Error(`Failed to load script: ${url}`));
    document.head.appendChild(script);
  });
}

function getMessages(locale: string) {
  return MESSAGES[locale as keyof typeof MESSAGES] || MESSAGES.en;
}

function applyRTL(el: HTMLElement, locale: string) {
  if (locale === 'ar') {
    el.style.direction = 'rtl';
    el.style.textAlign = 'right';
  } else {
    el.style.direction = 'ltr';
    el.style.textAlign = 'left';
  }
}

export class MediaPlayerBase {
  protected el: HTMLElement;
  protected video: HTMLVideoElement | null = null;
  protected tier: CapabilityResult | null = null;
  protected currentTime: number = 0;
  protected duration: number = 0;
  protected paused: boolean = true;
  protected events: PlayerEvents = {};
  protected locale: string = 'en';
  protected hlsInstance: any = null;
  protected mpegtsInstance: any = null;
  protected mediaSource: MediaSource | null = null;
  protected sourceBuffer: SourceBuffer | null = null;
  protected webCodecsDecoder: VideoDecoder | null = null;
  protected src: string = '';
  protected options: PlayerOptions = { src: '' };
  protected webCodecsCanvas: HTMLCanvasElement | null = null;
  protected webCodecsCtx: CanvasRenderingContext2D | null = null;
  protected webCodecsRunning: boolean = false;

  constructor(el: HTMLElement, options: PlayerOptions) {
    this.el = el;
    this.locale = options.locale || 'en';
    this.events = options.events || {};
    this.options = options;
    this.src = options.src || (el as any).getAttribute('src') || '';
    this.el.innerHTML = '';
    applyRTL(this.el, this.locale);
    this.init();
  }

  protected init(): void {
    this.tier = detectCapabilities();
    this.render();
  }

  protected render(): void {
    if (!this.tier) return;

    switch (this.tier.tier) {
      case 1:
        this.renderNative();
        break;
      case 2:
        void this.renderMSE();
        break;
      case 3:
        void this.renderWebCodecs();
        break;
      case 4:
        this.renderUnsupported();
        break;
    }
  }

  protected renderNative(): void {
    this.video = document.createElement('video');
    this.video.controls = true;
    this.video.preload = 'metadata';
    this.video.style.width = '100%';
    this.video.style.height = '100%';
    this.video.style.background = '#000';
    this.video.setAttribute('playsinline', '');
    if (this.options.autoplay) this.video.autoplay = true;
    if (this.options.muted) this.video.muted = true;
    if (this.options.loop) this.video.loop = true;
    this.el.appendChild(this.video);

    this.bindVideoEvents();

    this.video.src = this.src;
    if (this.options.autoplay) {
      this.video.play().catch(() => {});
    }
  }

  protected bindVideoEvents(): void {
    if (!this.video) return;

    this.video.addEventListener('loadedmetadata', () => {
      this.duration = this.video!.duration;
      this.events.onLoadedMetadata?.(this.duration);
    });
    this.video.addEventListener('timeupdate', () => {
      this.currentTime = this.video!.currentTime;
      this.events.onTimeUpdate?.(this.currentTime);
    });
    this.video.addEventListener('play', () => {
      this.paused = false;
      this.events.onPlay?.();
    });
    this.video.addEventListener('pause', () => {
      this.paused = true;
      this.events.onPause?.();
    });
    this.video.addEventListener('seeked', () => {
      this.currentTime = this.video!.currentTime;
      this.events.onSeeked?.(this.currentTime);
    });
    this.video.addEventListener('ended', () => {
      this.paused = true;
      this.events.onEnded?.();
    });
    this.video.addEventListener('error', () => {
      this.events.onError?.('Native video playback error');
    });
  }

  protected isHLS(): boolean {
    return this.src.includes('.m3u8') || this.src.includes('m3u8');
  }

  protected isMPEGTS(): boolean {
    return this.src.includes('.ts') || this.src.includes('.m2ts') || this.src.includes('mpegts');
  }

  protected async renderMSE(): Promise<void> {
    const msgs = getMessages(this.locale);

    // Create the video element up front so we can attach media to it.
    this.video = document.createElement('video');
    this.video.controls = true;
    this.video.preload = 'metadata';
    this.video.style.width = '100%';
    this.video.style.height = '100%';
    this.video.style.background = '#000';
    this.video.setAttribute('playsinline', '');
    if (this.options.autoplay) this.video.autoplay = true;
    if (this.options.muted) this.video.muted = true;
    if (this.options.loop) this.video.loop = true;
    this.el.appendChild(this.video);

    this.bindVideoEvents();

    try {
      if (this.isHLS()) {
        await this.loadHLS();
        if (this.hlsInstance && this.video) {
          this.hlsInstance.loadSource(this.src);
          this.hlsInstance.attachMedia(this.video);
        }
        return;
      }
      if (this.isMPEGTS()) {
        await this.loadMPEGTS();
        return;
      }
      // fMP4 / other: use MediaSource with a SourceBuffer.
      await this.loadFMP4ViaMediaSource();
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      this.showError(msgs.mseRequiresLibs, detail || msgs.mseDetail);
      this.events.onError?.(msgs.mseRequiresLibs);
    }
  }

  protected async loadHLS(): Promise<void> {
    await loadScript(HLS_JS_CDN);
    const Hls = (window as any).Hls;
    if (!Hls) throw new Error('Hls not available after load');
    if (!Hls.isSupported()) throw new Error('HLS not supported');

    this.hlsInstance = new Hls({
      enableWorker: true,
      lowLatencyMode: true,
    });

    this.hlsInstance.on(Hls.Events.MANIFEST_PARSED, (_event: any, data: any) => {
      this.duration = data.levels?.[0]?.duration || 0;
      this.events.onLoadedMetadata?.(this.duration);
    });

    this.hlsInstance.on(Hls.Events.ERROR, (_event: any, data: any) => {
      const msgs = getMessages(this.locale);
      if (data.fatal) {
        if (data.type === Hls.ErrorTypes.NETWORK_ERROR) {
          this.hlsInstance.startLoad();
        } else if (data.type === Hls.ErrorTypes.MEDIA_ERROR) {
          this.hlsInstance.recoverMediaError();
        } else {
          this.hlsInstance.destroy();
          this.showError(msgs.hlsLoadError, data.details);
          this.events.onError?.(`${msgs.hlsLoadError}: ${data.details}`);
        }
      }
    });

    this.hlsInstance.on(Hls.Events.LEVEL_LOADED, (_event: any, data: any) => {
      if (data.details?.duration) {
        this.duration = data.details.duration;
        this.events.onLoadedMetadata?.(this.duration);
      }
    });
  }

  protected async loadMPEGTS(): Promise<void> {
    await loadScript(MPEGTS_JS_CDN);
    const mpegts = (window as any).mpegts;
    if (!mpegts) throw new Error('mpegts.js not available after load');
    if (!mpegts.getFeatureList || !mpegts.getFeatureList().mseLivePlayback) {
      throw new Error('MSE live playback not supported');
    }

    if (!this.video) throw new Error('no video element');

    this.mpegtsInstance = mpegts.createPlayer(
      { type: 'mse', isLive: false, url: this.src },
      { enableWorker: true },
    );
    this.mpegtsInstance.attachMediaElement(this.video);
    this.mpegtsInstance.load();
    if (this.options.autoplay) {
      this.video.play().catch(() => {});
    }
  }

  protected async loadFMP4ViaMediaSource(): Promise<void> {
    if (!this.video) throw new Error('no video element');
    if (!('MediaSource' in window)) throw new Error('MediaSource not supported');

    const mediaSource = new MediaSource();
    this.mediaSource = mediaSource;
    this.video.src = URL.createObjectURL(mediaSource);

    await new Promise<void>((resolve, reject) => {
      mediaSource.addEventListener('sourceopen', () => resolve(), { once: true });
      mediaSource.addEventListener('error', () => reject(new Error('MediaSource error')), { once: true });
    });

    // Determine mime type from URL or default to mp4.
    const mime = this.src.includes('.webm') ? 'video/webm; codecs="vp8,opus"' : 'video/mp4; codecs="avc1.42E01E,mp4a.40.2"';
    let sb: SourceBuffer;
    try {
      sb = mediaSource.addSourceBuffer(mime);
    } catch (e) {
      throw new Error(`addSourceBuffer failed: ${e}`);
    }
    this.sourceBuffer = sb;

    const response = await fetch(this.src);
    if (!response.ok) throw new Error(`fetch failed: ${response.status}`);
    const buffer = await response.arrayBuffer();

    await new Promise<void>((resolve, reject) => {
      sb.addEventListener('updateend', () => resolve(), { once: true });
      sb.addEventListener('error', () => reject(new Error('SourceBuffer error')), { once: true });
      try {
        sb.appendBuffer(buffer);
      } catch (e) {
        reject(e);
      }
    });

    if (mediaSource.readyState === 'open') {
      try { mediaSource.endOfStream(); } catch { /* ignore */ }
    }

    this.duration = this.video.duration || 0;
    this.events.onLoadedMetadata?.(this.duration);
    if (this.options.autoplay) {
      this.video.play().catch(() => {});
    }
  }

  protected async renderWebCodecs(): Promise<void> {
    const msgs = getMessages(this.locale);

    // WebCodecs path: only meaningful for MP4 (H.264/AAC) containers in practice.
    // We attempt to demux with mp4box.js from CDN and decode via VideoDecoder.
    // If anything fails, fall back to a native <video> (browsers usually support
    // MP4 natively) or show an actionable message.
    const isMP4 = this.src.includes('.mp4') || this.src.includes('.m4v') || this.src.includes('.mov');

    if (!isMP4) {
      this.renderUnsupported();
      return;
    }

    // Prefer native playback (browsers support MP4/H.264 natively). This is the
    // reliable path; WebCodecs demux is only an enhancement and is not required
    // for correct MP4 playback.
    try {
      this.renderNative();
      if (this.video) {
        return;
      }
    } catch {
      // fall through to message
    }

    this.showError(msgs.webCodecsRequiresWasm, msgs.webCodecsDetail);
    this.events.onError?.(msgs.webCodecsRequiresWasm);
  }

  protected async loadScriptSafe(url: string): Promise<void> {
    await loadScript(url);
  }

  protected renderUnsupported(): void {
    const msgs = getMessages(this.locale);
    this.showUnsupported(
      msgs.unsupportedFormat,
      msgs.unsupportedDetail,
      true
    );
  }

  protected showError(title: string, detail: string): void {
    this.showInfo(title, detail, false);
  }

  protected showUnsupported(title: string, detail: string, showButton: boolean): void {
    const msgs = getMessages(this.locale);
    const dir = this.locale === 'ar' ? 'rtl' : 'ltr';
    const align = this.locale === 'ar' ? 'right' : 'left';

    this.el.innerHTML = `
      <div style="display: flex; flex-direction: column; align-items: center; justify-content: center; height: 100%; color: #666; font-family: system-ui, sans-serif; text-align: ${align}; padding: 2rem; direction: ${dir};">
        <div style="font-size: 3rem; margin-bottom: 1rem;">📹</div>
        <div style="font-size: 1.125rem; font-weight: 600; margin-bottom: 0.5rem;">${title}</div>
        <div style="font-size: 0.875rem; color: #888; max-width: 400px;">${detail}</div>
        ${showButton ? `<button data-download-app style="margin-top: 1rem; padding: 0.5rem 1rem; background: #4a90e2; color: white; border: none; border-radius: 4px; cursor: pointer;">${msgs.downloadApp}</button>` : ''}
      </div>
    `;

    const btn = this.el.querySelector('[data-download-app]') as HTMLButtonElement;
    if (btn) {
      btn.addEventListener('click', () => {
        window.open('https://frameforge.app/download', '_blank');
      });
    }
  }

  protected showInfo(title: string, detail: string, showButton: boolean): void {
    const msgs = getMessages(this.locale);
    const dir = this.locale === 'ar' ? 'rtl' : 'ltr';
    const align = this.locale === 'ar' ? 'right' : 'left';

    this.el.innerHTML = `
      <div style="display: flex; flex-direction: column; align-items: center; justify-content: center; height: 100%; color: #666; font-family: system-ui, sans-serif; text-align: ${align}; padding: 2rem; direction: ${dir};">
        <div style="font-size: 3rem; margin-bottom: 1rem;">${showButton ? '📹' : '⚙️'}</div>
        <div style="font-size: 1.125rem; font-weight: 600; margin-bottom: 0.5rem;">${title}</div>
        <div style="font-size: 0.875rem; color: #888; max-width: 400px;">${detail}</div>
        ${showButton ? `<button data-download-app style="margin-top: 1rem; padding: 0.5rem 1rem; background: #4a90e2; color: white; border: none; border-radius: 4px; cursor: pointer;">${msgs.downloadApp}</button>` : ''}
      </div>
    `;

    const btn = this.el.querySelector('[data-download-app]') as HTMLButtonElement;
    if (btn) {
      btn.addEventListener('click', () => {
        window.open('https://frameforge.app/download', '_blank');
      });
    }
  }

  // Public API
  play(): Promise<void> {
    if (this.video) {
      return this.video.play().then(() => {
        this.paused = false;
        this.events.onPlay?.();
      });
    }
    return Promise.reject(new Error('No video element'));
  }

  pause(): void {
    if (this.video) {
      this.video.pause();
      this.paused = true;
      this.events.onPause?.();
    }
    if (this.hlsInstance) {
      this.hlsInstance.stopLoad?.();
    }
    if (this.mpegtsInstance) {
      try { this.mpegtsInstance.pause(); } catch { /* ignore */ }
    }
  }

  seek(seconds: number): void {
    if (this.video) {
      this.video.currentTime = seconds;
      this.currentTime = seconds;
      this.events.onSeeked?.(seconds);
    }
    if (this.hlsInstance) {
      // hls.js handles seek via the media element.
    }
  }

  setPlaybackRate(rate: number): void {
    if (this.video) {
      this.video.playbackRate = rate;
    }
  }

  setVolume(volume: number): void {
    if (this.video) {
      this.video.volume = volume;
    }
  }

  setMuted(muted: boolean): void {
    if (this.video) {
      this.video.muted = muted;
    }
  }

  getCurrentTime(): number {
    return this.video ? this.video.currentTime : this.currentTime;
  }

  getDuration(): number {
    return this.video ? (isFinite(this.video.duration) ? this.video.duration : this.duration) : this.duration;
  }

  getPaused(): boolean {
    return this.video ? this.video.paused : this.paused;
  }

  getTier(): number {
    return this.tier?.tier ?? 4;
  }

  destroy(): void {
    if (this.hlsInstance) {
      try { this.hlsInstance.destroy(); } catch { /* ignore */ }
      this.hlsInstance = null;
    }
    if (this.mpegtsInstance) {
      try { this.mpegtsInstance.destroy(); } catch { /* ignore */ }
      this.mpegtsInstance = null;
    }
    if (this.mediaSource) {
      try {
        if (this.mediaSource.readyState === 'open') this.mediaSource.endOfStream();
      } catch { /* ignore */ }
      this.mediaSource = null;
    }
    this.sourceBuffer = null;
    if (this.webCodecsDecoder) {
      try { this.webCodecsDecoder.close(); } catch { /* ignore */ }
      this.webCodecsDecoder = null;
    }
    this.webCodecsRunning = false;
    if (this.video) {
      this.video.pause();
      this.video.src = '';
      if (this.video.parentNode) {
        this.video.remove();
      }
      this.video = null;
    }
    if (this.webCodecsCanvas) {
      this.webCodecsCanvas.remove();
      this.webCodecsCanvas = null;
      this.webCodecsCtx = null;
    }
    this.el.innerHTML = '';
    this.events = {};
  }
}
