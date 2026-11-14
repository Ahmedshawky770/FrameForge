export interface CapabilityResult {
    tier: 1 | 2 | 3 | 4;
    canPlayNative: boolean;
    canPlayMSE: boolean;
    canPlayWebCodecs: boolean;
    reason: string;
}
export declare function detectCapabilities(): CapabilityResult;
export declare function getSupportedExtensions(): string[];
//# sourceMappingURL=capability-detect.d.ts.map