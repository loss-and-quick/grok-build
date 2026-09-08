import { describe, expect, test } from "bun:test";
import { ANIMATION } from "@grok-build/theme";

import {
  pulseBrightness,
  TICKS_PER_SECOND,
  tickAt,
  waitingBrightness,
  waveBrightness,
} from "../src/animation.ts";

const source = await Bun.file(new URL("../src/animation.ts", import.meta.url)).text();

/** Extremes of a curve over one of its periods, sampled finely enough to reach both. */
function span(curve: (tick: number) => number, period: number): { min: number; max: number } {
  let min = Number.POSITIVE_INFINITY;
  let max = Number.NEGATIVE_INFINITY;
  for (let step = 0; step <= 2000; step += 1) {
    const value = curve((step / 2000) * period);
    min = Math.min(min, value);
    max = Math.max(max, value);
  }
  return { min, max };
}

describe("the pager's animation", () => {
  test("no timing is written here; every one comes from the generated artifact", () => {
    // This module used to transcribe the pager's speeds, which is the drift the
    // generator exists to prevent, and a comment saying so does not survive the
    // next edit. What is left is structural rather than a timing: milliseconds
    // per second, the 0-1-to-percent clamp, and an animation-frame handle.
    const structural = new Set(["0", "1", "100", "1000"]);
    const body = source.replace(/\/\*\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
    const literals = body.match(/(?<![\w.])\d+(?:\.\d+)?/g) ?? [];
    expect(literals.filter((literal) => !structural.has(literal))).toEqual([]);
  });

  test("a tick is a pager frame, so a wall-clock second is one cadence's worth", () => {
    expect(TICKS_PER_SECOND).toBe(ANIMATION.ticks_per_second);
    expect(tickAt(1000) - tickAt(0)).toBe(ANIMATION.ticks_per_second);
  });

  test("the waiting pulse sits on a floor: it dims but never goes dark", () => {
    // The floor is the whole point of the "waiting on you" curve — it is what
    // reads as paused rather than stopped — so it is asserted, not assumed.
    const { min, max } = span(waitingBrightness, Math.PI / ANIMATION.waiting_pulse_speed);
    expect(min).toBeCloseTo(ANIMATION.waiting_floor, 5);
    expect(max).toBeCloseTo(ANIMATION.waiting_floor + ANIMATION.waiting_range, 5);
  });

  test("the running wave has no floor, which is what makes it read as motion", () => {
    const { min, max } = span(waveBrightness, Math.PI / ANIMATION.wave_speed);
    expect(min).toBeCloseTo(0, 5);
    expect(max).toBeCloseTo(1, 5);
  });

  test("the wave is the shared pulse curve at the wave's own speed", () => {
    for (const tick of [0, 3.5, 17, 91.25]) {
      expect(waveBrightness(tick)).toBe(pulseBrightness(tick, ANIMATION.wave_speed));
    }
  });
});
