// The pager's glyphs, so the two clients read as one product.
//
// Nothing here is a codepoint. Every character comes from `GLYPHS` in
// `@grok-build/theme`, serialized from `xai-grok-pager-render/src/glyphs/` by
// calling the very functions the pager calls, and guarded by
// `glyphs::tokens::tests::generated_glyph_tokens_match_the_pager_glyphs`, which
// compares the artifact rather than rewriting it.
//
// This module is the naming layer: the artifact's keys are the pager's function
// names, which say what the terminal draws with a glyph, and the names below say
// what *this* client draws with it. Where the two uses differ that difference is
// the comment, and it is the only thing here worth reading.
//
// `test/glyphs.test.ts` fails if a literal glyph or a bare number reappears
// below, which is how the old hand-copied version of this file started.
import { GLYPHS } from "@grok-build/theme";

/**
 * `❯` — the prompt prefix, and the marker on a selected row.
 *
 * The artifact carries the pager's own `"❯ "`: two terminal columns, the glyph
 * plus its pad. A terminal pays for spacing in cells and this client pays for it
 * in CSS, so the pad is dropped here rather than turned into a space nobody can
 * style.
 */
export const PROMPT_ARROW = GLYPHS.prompt_arrow.trimEnd();

/** `◆` — the bullet on a tool call and on a collapsed thinking block. */
export const BULLET = GLYPHS.diamond_filled;

/**
 * `◇` — its hollow partner: free capacity in the context bar.
 *
 * The pair is the whole legend of that bar in the terminal, and it carries the
 * same meaning here: `◆` is a band of the window that is spent, `◇` is what is
 * left of it.
 */
export const DIAMOND_HOLLOW = GLYPHS.diamond_hollow;

/**
 * `◈` — the verb-group header diamond, and the context bar's informational row.
 *
 * The second use is the load-bearing one: the pager reserves this glyph for
 * rows that do **not** partition the window, so a reader can tell at a glance
 * which rows add up to what is used and which are already counted inside one of
 * them.
 */
export const GROUP_DIAMOND = GLYPHS.diamond_dotted;

/** `┃` — the left accent rail down a block. */
export const ACCENT_BAR = GLYPHS.accent_bar;

/** `│` — the separator between status chips. */
export const CHIP_SEPARATOR = GLYPHS.chip_separator;

/**
 * `›` — the pager's settings-breadcrumb separator, and so the separator a path's
 * breadcrumbs use here.
 */
export const CHEVRON = GLYPHS.chevron;

/** `‹` — its mirror: the pager's "previous", and here the way up a path. */
export const CHEVRON_LEFT = GLYPHS.chevron_left;

/** `▾` — the expanded-section marker; here, a subagent row showing its detail. */
export const DISCLOSURE_OPEN = GLYPHS.disclosure_open;

/** `▸` — the collapsed-section marker; here, a directory you can descend into. */
export const DISCLOSURE_CLOSED = GLYPHS.disclosure_closed;

/** `●` — the selected option marker. */
export const DOT_FILLED = GLYPHS.filled_dot;

/** `○` — its unselected partner. */
export const DOT_HOLLOW = GLYPHS.hollow_dot;

/** `✓` — the done marker. */
export const CHECK_MARK = GLYPHS.check_mark;

/** `✗` — its failure sibling, and the pager's own stop button. */
export const BALLOT_X = GLYPHS.ballot_x;

/** Braille spinner frames, in the pager's order. */
export const SPINNER_FRAMES = GLYPHS.braille_spinner_frames;

/** Ticks a spinner frame is held for. */
export const SPINNER_DIVISOR = GLYPHS.spinner_divisor;

export function spinnerFrame(tick: number): string {
  const at = Math.floor(tick / SPINNER_DIVISOR) % SPINNER_FRAMES.length;
  return SPINNER_FRAMES[at] ?? SPINNER_FRAMES[0];
}
