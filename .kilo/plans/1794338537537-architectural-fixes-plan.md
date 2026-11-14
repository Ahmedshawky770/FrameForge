# Architectural Fixes Plan for FrameForge

## Overview
Fix remaining architectural conflicts and inconsistencies (🔵 issues) identified in the evaluation.

## Issues to Fix

### 1. Web Component `<media-player>` Missing (P1)
**Location:** `packages/media-player/` - only package.json and rollup config exist
**Problem:** Tiered strategy (native `<video>` → MSE remux → WebCodecs + WASM demuxer → "unsupported") documented but not implemented
**Solution:** Implement the web component with:
- Lit or vanilla Custom Element
- Tier 1: Native `<video>` for MP4/WebM
- Tier 2: MSE (Media Source Extensions) for fragmented MP4/TS
- Tier 3: WebCodecs + WASM demuxer (ffmpeg.wasm or custom)
- Tier 4: Graceful fallback message
- RTL/Arabic support
- TypeScript definitions

### 2. Monolithic main.rs (P1)
**Location:** `apps/tauri-app/src-tauri/main.rs` (768 lines)
**Problem:** All Tauri commands, state management, setup in one file
**Solution:** Split into modules:
- `commands/player.rs` - player commands
- `commands/scrub.rs` - scrub commands
- `commands/project.rs` - project/export commands
- `commands/subtitle.rs` - subtitle commands
- `commands/locale_theme.rs` - i18n/theme commands
- `state.rs` - all State structs
- `setup.rs` - Tauri setup logic
- `mpris.rs` - already separate but needs integration

### 3. ADR-2 Alignment Verification (P1)
**Location:** `specs/05-adrs.md` vs implementation
**Problem:** ADR-2 says "Strategy A preferred" but Wayland uses Strategy B
**Solution:** Update ADR-2 to document Strategy B as the Wayland fallback, with rationale and verification criteria.

## Implementation Order
1. Split main.rs into modules (foundational for maintainability)
2. Implement web component (independent, can be done in parallel)
3. Update ADR-2 documentation

## Validation
- `cargo check --workspace` passes
- `cargo test --workspace` passes
- `npm run build` in packages/media-player succeeds
- Web component loads and plays video in browser
- ADR-2 accurately reflects implementation