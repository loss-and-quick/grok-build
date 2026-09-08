// The pager's glyphs, so the two clients read as one product.
//
// Copied from `xai-grok-pager-render/src/glyphs.rs`. Like the animation
// constants these are not generated — `glyphs.rs` is a Rust module of `const
// char`s with no artifact — and the same fix applies: emit them alongside the
// palette. They change far less often than colours, but "less often" is not
// "never", so `test/glyphs.test.ts` reads `glyphs.rs` and compares.
//
// Two below are pinned by nothing, because `glyphs.rs` does not own them: the
// chip separator lives in the context bar, and the hollow dot is a literal at
// the pager's own permission and question views. Naming that here is cheaper
// than a test that would have to know where every literal in the pager is.
//
// The ASCII fallbacks in `glyphs.rs` exist for legacy Windows consoles that
// cannot render the codepoint. A browser has no such limit, so only the real
// glyphs are here.

/** `❯ ` — the prompt prefix. `glyphs::prompt_arrow()`. */
export const PROMPT_ARROW = "❯";

/** `◆` — the bullet on a tool call and a collapsed thinking block. `diamond_filled()`. */
export const BULLET = "◆";

/** `◈` — the verb-group header diamond. `diamond_dotted()`. */
export const GROUP_DIAMOND = "◈";

/** `┃` — the left accent rail down a block. `accent_bar()`. */
export const ACCENT_BAR = "┃";

/** `│` — the separator between status chips. `context_bar::SEPARATOR`. */
export const CHIP_SEPARATOR = "│";

/**
 * `›` — the separator the pager already uses for **settings breadcrumbs**, and
 * so the one a path's breadcrumbs use here. `chevron()`.
 */
export const CHEVRON = "›";

/** `‹` — `chevron()`'s mirror: the pager's "previous", and here the way up. `chevron_left()`. */
export const CHEVRON_LEFT = "‹";

/** `▾` — the expanded-section marker; here, a subagent row showing its detail. `disclosure_open()`. */
export const DISCLOSURE_OPEN = "▾";

/** `▸` — the collapsed-section marker; here, a directory you can descend into. `disclosure_closed()`. */
export const DISCLOSURE_CLOSED = "▸";

/** `●` — the selected option marker. `filled_dot()`. */
export const DOT_FILLED = "●";

/** `○` — its unselected partner, a literal at `permission_view.rs` and `question_view.rs`. */
export const DOT_HOLLOW = "○";

/** `✓` — the done marker. `check_mark()`. */
export const CHECK_MARK = "✓";

/** `✗` — its failure sibling, and the pager's own stop button. `ballot_x()`. */
export const BALLOT_X = "✗";

/** Braille spinner frames, cycled every 4 ticks. `braille_spinner_frames()`. */
export const SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"] as const;

/** Ticks per spinner frame — `SPINNER_DIVISOR` in `turn_status.rs`, ~7.5fps. */
export const SPINNER_DIVISOR = 4;

export function spinnerFrame(tick: number): string {
  const at = Math.floor(tick / SPINNER_DIVISOR) % SPINNER_FRAMES.length;
  return SPINNER_FRAMES[at] ?? SPINNER_FRAMES[0];
}
