// The pager's glyphs, so the two clients read as one product.
//
// Copied from `xai-grok-pager-render/src/glyphs.rs`. Like the animation
// constants these are not generated — `glyphs.rs` is a Rust module of `const
// char`s with no artifact — and the same fix applies: emit them alongside the
// palette. They change far less often than colours, but "less often" is not
// "never", so `glyphsMatchThePager` pins them.
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

/** `●` / `○` — selected and unselected option markers. `filled_dot()`. */
export const DOT_FILLED = "●";
export const DOT_HOLLOW = "○";

/** Braille spinner frames, cycled every 4 ticks. `braille_spinner_frames()`. */
export const SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"] as const;

/** Ticks per spinner frame — `SPINNER_DIVISOR` in `turn_status.rs`, ~7.5fps. */
export const SPINNER_DIVISOR = 4;

export function spinnerFrame(tick: number): string {
  const at = Math.floor(tick / SPINNER_DIVISOR) % SPINNER_FRAMES.length;
  return SPINNER_FRAMES[at] ?? SPINNER_FRAMES[0];
}
