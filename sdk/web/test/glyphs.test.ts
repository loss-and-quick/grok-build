import { describe, expect, test } from "bun:test";
import { GLYPHS } from "@grok-build/theme";

import {
  ACCENT_BAR,
  BALLOT_X,
  BULLET,
  CHECK_MARK,
  CHEVRON,
  CHEVRON_LEFT,
  CHIP_SEPARATOR,
  DISCLOSURE_CLOSED,
  DISCLOSURE_OPEN,
  DOT_FILLED,
  DOT_HOLLOW,
  GROUP_DIAMOND,
  PROMPT_ARROW,
  SPINNER_DIVISOR,
  SPINNER_FRAMES,
  spinnerFrame,
} from "../src/glyphs.ts";

const source = await Bun.file(new URL("../src/glyphs.ts", import.meta.url)).text();

/** The module with its comments removed, so a guard reads code and not prose. */
function code(text: string): string {
  return text.replaceAll(/\/\*\*[\s\S]*?\*\//g, "").replaceAll(/^\s*\/\/.*$/gm, "");
}

describe("the glyphs are the pager's, and stay the pager's", () => {
  test("no codepoint is written here; every one comes from the generated artifact", () => {
    // This module used to be a hand-copied transcription of `glyphs.rs`, checked
    // by a test in this directory that scraped the Rust with a regular
    // expression — which could only ever compare the glyphs somebody had
    // already thought to list. The artifact replaced it, and this is what keeps
    // a transcription from creeping back: outside the doc comments, which name
    // each glyph so a reader knows which one a constant is, the body is ASCII.
    const written = [...code(source)].filter((ch) => (ch.codePointAt(0) ?? 0) > 127);
    expect(written).toEqual([]);
  });

  test("no cadence is written here either", () => {
    // `SPINNER_DIVISOR` was a transcribed `4` for the same reason the glyphs
    // were transcribed. What is left is structural rather than a cadence: the
    // first frame, standing in for an index that cannot be out of range.
    const structural = new Set(["0"]);
    const literals = code(source).match(/(?<![\w.])\d+(?:\.\d+)?/g) ?? [];
    expect(literals.filter((literal) => !structural.has(literal))).toEqual([]);
  });

  test.each([
    ["prompt_arrow", PROMPT_ARROW],
    ["diamond_filled", BULLET],
    ["diamond_dotted", GROUP_DIAMOND],
    ["accent_bar", ACCENT_BAR],
    ["chip_separator", CHIP_SEPARATOR],
    ["filled_dot", DOT_FILLED],
    ["hollow_dot", DOT_HOLLOW],
    ["chevron", CHEVRON],
    ["chevron_left", CHEVRON_LEFT],
    ["disclosure_closed", DISCLOSURE_CLOSED],
    ["disclosure_open", DISCLOSURE_OPEN],
    ["check_mark", CHECK_MARK],
    ["ballot_x", BALLOT_X],
  ] as const)("`%s` is what this client draws", (key, ours) => {
    // `prompt_arrow` is the one glyph whose artifact value is not what a browser
    // draws: it carries the pager's own trailing pad, which is two terminal
    // columns, and CSS supplies the spacing here. Nothing else is transformed,
    // and this asserts that.
    const expected = key === "prompt_arrow" ? GLYPHS[key].trimEnd() : GLYPHS[key];
    expect(ours).toBe(expected);
  });

  test("the spinner is the artifact's frames, in the artifact's order", () => {
    expect([...SPINNER_FRAMES]).toEqual([...GLYPHS.braille_spinner_frames]);
    expect(SPINNER_DIVISOR).toBe(GLYPHS.spinner_divisor);
  });

  test("a frame is held for the pager's dwell, and the cycle wraps", () => {
    // The dwell is what makes the two clients spin at the same speed rather than
    // merely through the same glyphs, so it is asserted as behaviour and not
    // only as a number.
    for (let frame = 0; frame < SPINNER_FRAMES.length; frame += 1) {
      const first = frame * SPINNER_DIVISOR;
      expect(spinnerFrame(first)).toBe(SPINNER_FRAMES[frame]!);
      expect(spinnerFrame(first + SPINNER_DIVISOR - 1)).toBe(SPINNER_FRAMES[frame]!);
    }
    expect(spinnerFrame(SPINNER_FRAMES.length * SPINNER_DIVISOR)).toBe(SPINNER_FRAMES[0]);
  });
});
