import { CapabilityResult } from './capability-detect';
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
export declare class MediaPlayerBase {
    protected el: HTMLElement;
    protected video: HTMLVideoElement | null;
    protected tier: CapabilityResult | null;
    protected currentTime: number;
    protected duration: number;
    protected paused: boolean;
    protected events: PlayerEvents;
    protected locale: string;
    protected hlsInstance: any;
    protected muxInstance: any;
    protected src: string;
    protected options: PlayerOptions;
    constructor(el: HTMLElement, options: PlayerOptions);
    protected init(): void;
    protected render(): void;
    protected renderNative(): void;
    protected bindVideoEvents(): void;
    protected renderMSE(): Promise<void>;
    protected loadHLS(): Promise<void>;
    protected loadMuxJS(): Promise<void>;
    protected getOrCreateSourceBuffer(mimeType: string): SourceBuffer | null;
    protected renderWebCodecs(): void;
    protected renderUnsupported(): void;
    protected showMSEUnsupported(title: string): void;
    protected showError(title: string, detail: string): void;
    protected showUnsupported(title: string, detail: string, showButton: boolean): void;
    protected showInfo(title: string, detail: string, showButton: boolean): void;
    play(): Promise<void>;
    pause(): void;
    seek(seconds: number): void;
    setPlaybackRate(rate: number): void;
    setVolume(volume: number): void;
    setMuted(muted: boolean): void;
    getCurrentTime(): number;
    getDuration(): number;
    getPaused(): boolean;
    getTier(): number;
    destroy(): void;
}
//# sourceMappingURL=tiers.d.ts.map