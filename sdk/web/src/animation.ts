// The pager's animation, read from the generated artifact.
//
// Nothing here spells out a speed or a curve constant, for the same reason
// nothing in `theme.ts` spells out a colour: `ANIMATION` is serialized from the
// Rust the pager itself animates on
// (`xai-grok-pager-render/src/theme/animation.rs`), so a tweak there fails
// `theme::tokens::tests::generated_theme_tokens_match_the_pager_palette` until
// it is regenerated, and lands here when it is.
//
// What the artifact deliberately does not carry is `wave_rows`. The pager
// paints a running rail as a *column of terminal cells* and offsets each cell's
// phase by its row; `wave_rows` is how many rows one full wave spans. A rail
// here is a single element, so it is the pager's row 0 and there is no row
// index to offset. `wave_rows` is therefore terminal geometry rather than a
// preference this client quietly fails to honour, which is also why it stays a
// `pager.toml` knob — `SettingSurface::Terminal` defines itself as exactly
// that, "a column width, the frame cadence".
import { createSignal, onCleanup, onMount } from "solid-js";
import { ANIMATION } from "@grok-build/theme";

/**
 * Frame cadence. Every speed below is radians *per tick*, so the cadence is
 * part of the timing.
 *
 * This is the pager's shipped default. `pager.toml` can retune the terminal's
 * own cadence, and nothing carries that to a browser — which the generated
 * artifact says in as many words.
 */
export const TICKS_PER_SECOND = ANIMATION.ticks_per_second;

/**
 * `sin²(tick·speed)` — the pager's `pulse_brightness`.
 *
 * Everything driven by one tick pulses in unison, which is what makes a row of
 * bullets read as one animation rather than several.
 */
export function pulseBrightness(tick: number, speed: number): number {
  const s = Math.sin(tick * speed);
  return s * s;
}

/**
 * The running accent wave: tool rails, running bullets, the running verb-group
 * diamond.
 *
 * The pager's `wave_brightness` at row 0. A rail here is one element, so there
 * is no per-row phase offset to apply — see the note at the top of this file.
 */
export function waveBrightness(tick: number): number {
  return pulseBrightness(tick, ANIMATION.wave_speed);
}

/**
 * The "waiting on you" pulse: the pager's `waiting_brightness`, a pulse lifted
 * onto a floor so the diamond dims without ever going dark.
 *
 * That floor is what separates *paused on you* from *still working*, which is
 * why it is a generated constant rather than a rounded-off literal.
 */
export function waitingBrightness(tick: number): number {
  return (
    ANIMATION.waiting_floor +
    pulseBrightness(tick, ANIMATION.waiting_pulse_speed) * ANIMATION.waiting_range
  );
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
 * A shared tick signal at the pager's cadence, started lazily and stopped when
 * nothing reads it.
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
