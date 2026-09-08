import { describe, expect, test } from "bun:test";

import {
  ACCENT_BAR,
  BALLOT_X,
  BULLET,
  CHECK_MARK,
  CHEVRON,
  CHEVRON_LEFT,
  DISCLOSURE_CLOSED,
  DISCLOSURE_OPEN,
  DOT_FILLED,
  GROUP_DIAMOND,
  PROMPT_ARROW,
  SPINNER_FRAMES,
} from "../src/glyphs.ts";

// `glyphs.ts` says these are the pager's own glyphs and that a test pins them.
// It said so before this file existed, which is precisely the kind of claim
// that rots: `glyphs.rs` is a Rust module of `const`s with no generated
// artifact, so nothing but a test standing here can keep the two in step.
const RUST = await Bun.file(
  new URL("../../../crates/codegen/xai-grok-pager-render/src/glyphs.rs", import.meta.url),
).text();

/** `\u{XXXX}` is Rust's escape and TypeScript's, but only one of them is data here. */
function decodeRustEscapes(literal: string): string {
  return literal.replaceAll(/\\u\{([0-9a-fA-F]+)\}/g, (_, hex: string) =>
    String.fromCodePoint(Number.parseInt(hex, 16)),
  );
}

function bodyOf(fn: string): string {
  const start = RUST.indexOf(`pub fn ${fn}(`);
  expect(start).toBeGreaterThan(-1);
  const end = RUST.indexOf("\n}\n", start);
  return RUST.slice(start, end);
}

/**
 * The glyph a modern terminal gets.
 *
 * Every one of these functions is `if is_legacy_windows_console() { … } else {
 * … }`, and only the `else` matters here: the ASCII fallbacks exist for legacy
 * ConHost, which has no font fallback. A browser always has one, so this client
 * carries the real glyph and nothing else.
 */
function modernGlyph(fn: string): string {
  const body = bodyOf(fn);
  const otherwise = body.slice(body.indexOf("} else {"));
  const literal = /"((?:[^"\\]|\\.)*)"/.exec(otherwise);
  expect(literal).not.toBeNull();
  return decodeRustEscapes(literal![1]!);
}

describe("the glyphs are the pager's, and stay the pager's", () => {
  test.each([
    ["prompt_arrow", PROMPT_ARROW],
    ["diamond_filled", BULLET],
    ["diamond_dotted", GROUP_DIAMOND],
    ["accent_bar", ACCENT_BAR],
    ["filled_dot", DOT_FILLED],
    ["chevron", CHEVRON],
    ["chevron_left", CHEVRON_LEFT],
    ["disclosure_closed", DISCLOSURE_CLOSED],
    ["disclosure_open", DISCLOSURE_OPEN],
    ["check_mark", CHECK_MARK],
    ["ballot_x", BALLOT_X],
  ])("`%s` is what this client draws", (fn, ours) => {
    // `prompt_arrow` returns `"❯ "` — two columns, the glyph plus its own pad.
    // A terminal pays for spacing in cells; here CSS does, so the pad is not
    // part of the constant and the comparison is on the glyph alone.
    expect(modernGlyph(fn).trimEnd()).toBe(ours);
  });

  test("the spinner is the same eight braille frames, in the same order", () => {
    const body = bodyOf("braille_spinner_frames");
    const fancy = body.slice(body.indexOf("const FANCY"), body.indexOf("const FALLBACK"));
    const frames = [...fancy.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => decodeRustEscapes(m[1]!));
    expect(frames).toEqual([...SPINNER_FRAMES]);
  });
});
