// Telling someone the agent is waiting on them when they are not looking.
//
// The terminal's user is looking at the terminal. A browser's may be in another
// tab, another window, or another room, and the client this replaces said
// nothing at all in that case: the tab was titled `grok — sessions` whether a
// turn was streaming, finished, or stopped dead on a permission request.
//
// ## What is worth interrupting for is not a new question
//
// The pager already answers it, and this takes the answer rather than inventing
// one. `NotificationConfig::default` enables exactly two events —
// `TurnComplete` and `ApprovalRequired` (`notifications/config.rs:30-33`) — out
// of the five it defines, and fires them on `NotificationCondition::Unfocused`
// (`:57-63`). Streaming tokens are not on that list, and neither is a tool
// starting. So: a blocked turn interrupts, a finished turn interrupts, and
// nothing else does.
//
// ## Two channels, and only one of them may be depended on
//
// The **title** is the baseline. It needs no permission, cannot be refused, and
// is the terminal's own answer: `TitleConfig::default` puts `ActionRequired`
// first, ahead of the session's own name (`config.rs:70-80`), and blinks it
// about once a second while the window is unfocused, standing still while it is
// focused (`title.rs:184-197`). A browser tab title is a window title, so it
// carries the same words in the same order.
//
// The **Notification API** is the interrupt, and it is strictly extra. It can be
// denied, it can be missing entirely on an insecure origin, and asking for it
// needs a gesture. Nothing here changes behaviour when it is unavailable — the
// title has already said everything by then.
import { TICKS_PER_SECOND } from "./animation.ts";
import { GLYPH_WARNING } from "./glyphs.ts";

/**
 * Ticks the `⚠ Action Required` flag is held for before it toggles.
 *
 * Derived rather than typed: the pager holds it 15 ticks at 30 ticks a second
 * for "a calm 1s blink cycle that reads as intentional rather than broken
 * flickering" (`title.rs:19-23`). Half the artifact's cadence *is* that number,
 * so a retune of the cadence moves both clients together instead of leaving one
 * behind at a hard-coded 15.
 */
export const BLINK_TICKS = TICKS_PER_SECOND / 2;

/** The pager's own truncation for a session name in a title (`title.rs:150`). */
const NAME_LIMIT = 40;

export interface TitleFacts {
  /** Unanswered questions this browser is holding. */
  waiting: number;
  /** A turn is running. */
  busy: boolean;
  /** What the session is called, or `null` for one with no name yet. */
  sessionName: string | null;
  /** Whether this tab is the one being looked at. */
  focused: boolean;
  /** Which half of the blink cycle we are in. Ignored while focused. */
  blinkOn: boolean;
}

/**
 * The tab title, composed the way the terminal composes a window title.
 *
 * Same items in the same order, same `" - "` separator, same `"grok"` when
 * nothing else has anything to say.
 *
 * **One item is deliberately dropped: the spinner.** The terminal animates a
 * braille frame there because a title is the only place a backgrounded session
 * shows a pulse, and it pays for that with a comment about how often a tab bar
 * can be repainted. A browser has the same debounce problem and one extra
 * reason not to: a title that animates for decoration is exactly what a reader
 * who asked for reduced motion asked not to have, and the item beside it —
 * the activity word — carries the same "still alive" meaning with information
 * in it. So `Activity` stays and `Spinner` goes.
 */
export function composeTitle(facts: TitleFacts): string {
  const parts: string[] = [];
  // First, ahead of the session's own name, because that is where the terminal
  // puts it: what is blocked outranks what it is called.
  if (facts.waiting > 0 && (facts.focused || facts.blinkOn)) {
    parts.push(
      facts.waiting === 1
        ? `${GLYPH_WARNING} Action Required`
        : `${GLYPH_WARNING} Action Required (${facts.waiting})`,
    );
  }
  if (facts.busy) parts.push("Working");
  const name = facts.sessionName?.trim();
  if (name) parts.push(name.length > NAME_LIMIT ? `${name.slice(0, NAME_LIMIT - 1)}…` : name);
  parts.push("grok");
  return parts.join(" - ");
}

/** Whether this browser can even offer to interrupt. */
export function canNotify(): boolean {
  return typeof Notification !== "undefined";
}

/**
 * What the out-of-app channel is currently allowed to do.
 *
 * `"unavailable"` and `"denied"` are the same outcome — nothing is shown — but
 * not the same sentence to a person: one is worth a button and the other is
 * not.
 */
export type NotifyPermission = "unavailable" | "default" | "granted" | "denied";

export function notifyPermission(): NotifyPermission {
  if (!canNotify()) return "unavailable";
  return Notification.permission as NotifyPermission;
}

/**
 * Ask, from inside a gesture.
 *
 * Never called on load. A permission prompt nobody asked for is the thing that
 * teaches people to press Block, and Block is permanent.
 */
export async function askToNotify(): Promise<NotifyPermission> {
  if (!canNotify()) return "unavailable";
  try {
    return (await Notification.requestPermission()) as NotifyPermission;
  } catch {
    return "denied";
  }
}

/** One notification, or nothing at all — never an exception. */
export function raise(title: string, body: string, tag: string): void {
  if (notifyPermission() !== "granted") return;
  try {
    // `tag` lets the platform replace rather than stack: five permissions in a
    // row is one notification that keeps changing, not five to dismiss.
    new Notification(title, { body, tag });
  } catch {
    // A browser may refuse to construct one (a page without a service worker on
    // some mobile builds). The title has already said it.
  }
}

/**
 * Should this event interrupt?
 *
 * The terminal's `NotificationCondition::Unfocused` default, transcribed: only
 * when the window is not the one being looked at. Someone watching the turn
 * happen does not need to be told it happened.
 */
export function shouldInterrupt(focused: boolean): boolean {
  return !focused;
}

/** Whether this browser has been asked to hold still. */
export function reducedMotion(): boolean {
  if (typeof matchMedia !== "function") return false;
  try {
    return matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
}
