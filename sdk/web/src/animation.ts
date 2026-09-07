import { createSignal, onCleanup, onMount } from "solid-js";

// The pager's animation, reproduced.
//
// ## Why these numbers are written here and not imported
//
// The palette is generated from the pager's Rust, which is why no colour is
// spelled out anywhere in this package. Animation is not generated:
// `theme/tokens.rs` excludes it on purpose — "`wave_brightness` /
// `pulse_brightness` are behavior over a tick counter, not palette" — so there
// is nothing to import, and the constants below are transcribed from named
// Rust constants rather than derived from an artifact.
//
// That is a gap, not a decision I am happy with, and the fix is the same one
// that fixed the palette: emit them. It needs a small Rust move first, because
// the curves live in `xai-grok-pager-render` while their speeds live one crate
// up in `xai-grok-pager`:
//
//   - `WAVE_SPEED` — scrollback/wrappers/entry_renderer.rs
//   - `USER_WAITING_PULSE_SPEED` — views/turn_status.rs
//
// Move those beside `wave_brightness`/`pulse_brightness` in
// `xai-grok-pager-render/src/theme/`, have the pager import them, and
// `tokens.rs` can then emit an `ANIMATION` block guarded by the same
// compare-never-rewrite test as the colours. Until that lands, a pager edit
// silently desynchronises this file, which is exactly the failure the theme
// generator exists to prevent — so `animationConstantsMatchThePager` pins them
// and this comment names the source lines.

/**
 * Frame cadence. The pager's tick counter advances at roughly 30fps, and every
 * speed below is radians *per tick*, so the cadence is part of the timing.
 */
export const TICKS_PER_SECOND = 30;

/**
 * Radians per tick for the accent wave: running tool rails, running bullets and
 * the running verb-group diamond.
 *
 * `WAVE_SPEED` in `entry_renderer.rs`.
 */
export const WAVE_SPEED = 0.15;

/**
 * Radians per tick for the "waiting on you" pulse on the turn-status diamond.
 *
 * `USER_WAITING_PULSE_SPEED` in `turn_status.rs`; about a 1.3s cycle at 30fps.
 */
export const PULSE_SPEED = 0.08;

/**
 * Rows per full wave cycle, so the wave travels down a block at a fixed rate
 * regardless of its height.
 *
 * `AnimationConfig::wave_rows` default in `appearance/config.rs`. It is a user
 * setting in the pager and has no wire form, so a browser cannot read the
 * user's value — see the settings finding.
 */
export const WAVE_ROWS = 32;

/**
 * `sin²(tick·speed + 2π·row/waveRows)` — the pager's `wave_brightness`.
 *
 * Each row carries a fixed phase offset, which is what makes the brightness
 * travel down a block rather than blink in unison.
 */
export function waveBrightness(tick: number, row: number, waveRows = WAVE_ROWS, speed = WAVE_SPEED): number {
  const phase = (row / Math.max(1, waveRows)) * 2 * Math.PI;
  const s = Math.sin(tick * speed + phase);
  return s * s;
}

/** `sin²(tick·speed)` — the pager's `pulse_brightness`; everything on one tick pulses together. */
export function pulseBrightness(tick: number, speed = PULSE_SPEED): number {
  const s = Math.sin(tick * speed);
  return s * s;
}

/**
 * The pager's "waiting on you" diamond floor: `0.3 + sin²(…)·0.7`, so it dims
 * without ever going dark. `pending_diamond_color` in `turn_status.rs`.
 */
export function waitingBrightness(tick: number): number {
  return 0.3 + pulseBrightness(tick) * 0.7;
}

/**
 * One shared tick, read as a signal.
 *
 * Derived from wall-clock rather than counted per frame, so a page that is
 * throttled in a background tab resumes in phase with the terminal instead of
 * lagging by however many frames it missed.
 */
export function tickAt(nowMs: number): number {
  return (nowMs / 1000) * TICKS_PER_SECOND;
}

/**
 * A shared 30fps tick signal, started lazily and stopped when nothing reads it.
 *
 * One `requestAnimationFrame` loop for the whole page: everything animated in
 * the pager shares a single tick counter, and sharing it here is what keeps a
 * row of bullets pulsing in unison rather than each on its own phase.
 */
export function createTick(): () => number {
  let frame = 0;
  const [tick, setTick] = createSignal(tickAt(performance.now()));
  const loop = (): void => {
    setTick(tickAt(performance.now()));
    frame = requestAnimationFrame(loop);
  };
  onMount(() => {
    frame = requestAnimationFrame(loop);
  });
  onCleanup(() => cancelAnimationFrame(frame));
  return tick;
}

/**
 * Blend from a background toward a colour, the way the pager's `blend_color`
 * does: a per-channel lerp where `0` is the background and `1` is the colour.
 *
 * Expressed as `color-mix` so the inputs stay CSS custom properties and no
 * channel arithmetic happens here — the palette is still the only source of
 * the two endpoints.
 */
export function blendToward(background: string, color: string, brightness: number): string {
  const pct = Math.round(Math.max(0, Math.min(1, brightness)) * 100);
  return `color-mix(in srgb, ${color} ${pct}%, ${background})`;
}
