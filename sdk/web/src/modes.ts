// Session modes, and the two very different things that word means here.
//
// ## They exist on the wire, and one of them is undiscoverable
//
// Checked against a live agent rather than against the schema. `session/set_mode`
// is answered `{}` and the agent broadcasts `session/update` with
// `{sessionUpdate: "current_mode_update", currentModeId: …}` to every
// subscriber, so a terminal and a browser on one session move together.
//
// What the agent never sends is the *list*. `initialize` carries no `modes`,
// and neither does `session/new` — both were probed and both answered `null`.
// So a client cannot discover which ids exist; it has to know them, which is
// what the pager does too.
//
// Worse, and this is why the list below is short: **`set_mode` accepts an id it
// does not implement.** `plan` and `default` are answered *and broadcast*;
// `ask`, `acceptEdits`, `bypassPermissions` and the literal string `nonsense`
// are all answered `{}` with no broadcast at all. An accepted-and-ignored id is
// the worst outcome available, because a control built on one looks like it
// worked. Hence two rules here: only the two ids that were seen to take effect
// are offered, and the mode on screen is only ever the one the agent has
// *confirmed*, never the one that was asked for.
//
// ## The other mode is not this mode
//
// How much the agent asks permission for does **not** ride `session/set_mode`.
// It rides an extension notification, `_x.ai/yolo_mode_changed`, carrying
// `{sessionId, yolo_mode, auto_mode, permission_mode}` — and it is not a detail.
// With `auto_mode` on, which is the default wherever an auto-mode classifier is
// configured, an LLM answers every `session/request_permission` before any
// client sees it: a `rm -rf` outside the workspace ran unprompted, and the same
// request produced a permission card the moment `auto_mode: false` was sent.
// A browser that cannot send it is a browser that can never be asked anything.

/** A mode this client has watched take effect. */
export interface SessionMode {
  id: string;
  label: string;
  description: string;
}

/**
 * The modes `session/set_mode` was observed to actually enter.
 *
 * Deliberately not "every id the pager has a variant for": an id the agent
 * accepts and drops would be a control that lies.
 */
export const SESSION_MODES: readonly SessionMode[] = [
  {
    id: "default",
    label: "Normal",
    description: "The agent edits, runs commands and asks when it needs to.",
  },
  {
    id: "plan",
    label: "Plan",
    description: "The agent works out what it would do and writes nothing.",
  },
];

export function modeById(id: string | null): SessionMode | null {
  return SESSION_MODES.find((mode) => mode.id === id) ?? null;
}

/**
 * How much the agent decides on its own, as the three states a person means.
 *
 * The names are the wire's own fields rather than an invented scale: `yolo_mode`
 * and `auto_mode` are two booleans on `_x.ai/yolo_mode_changed`, and the three
 * combinations below are the three that mean something.
 */
export type PermissionMode = "ask" | "auto" | "always-approve";

export interface PermissionModeChoice {
  id: PermissionMode;
  label: string;
  description: string;
  /**
   * Whether choosing it should be confirmed before it takes effect.
   *
   * True for exactly one, and the reason is the same reason a permission
   * request is a modal at all: this is the switch that stops the agent asking.
   * Every other mode change can be undone by making another one; the turns that
   * ran unasked while this was on cannot.
   */
  confirm: boolean;
}

export const PERMISSION_MODES: readonly PermissionModeChoice[] = [
  {
    id: "ask",
    label: "Ask me",
    description: "Every tool that edits, runs or fetches stops and asks.",
    confirm: false,
  },
  {
    id: "auto",
    label: "Decide the easy ones",
    description: "A classifier answers what it judges safe and asks about the rest.",
    confirm: false,
  },
  {
    id: "always-approve",
    label: "Never ask",
    description: "Nothing stops. The agent edits and runs commands unprompted.",
    confirm: true,
  },
];

/**
 * The `_x.ai/yolo_mode_changed` params for a choice, in the agent's own vocabulary.
 *
 * Two things here were got wrong first and are worth stating, because both were
 * invisible: the notification carries **no session id** — the pager sends none,
 * and its own comment says the agent "applies it to every session of the sending
 * client", which the leader log confirms by reporting `target_sessions` equal to
 * this client's session count. And `permission_mode` is one of `ask`, `auto` or
 * `always-approve`; a plausible-looking `"default"` is accepted by the socket
 * and changes nothing.
 *
 * `yolo_mode` is sent on every choice rather than omitted, and that is a
 * deliberate difference from the pager's auto kill-switch, which omits it so a
 * sibling tab's always-approve survives. This is not a kill-switch: it is a
 * person picking the posture, and "Ask me" that quietly left never-asking on
 * would be the worst possible outcome of a control about being asked.
 */
export function permissionModeParams(mode: PermissionMode): Record<string, unknown> {
  return {
    yolo_mode: mode === "always-approve",
    auto_mode: mode === "auto",
    permission_mode: mode,
  };
}

/**
 * Read a mode back out of what the roster says.
 *
 * The roster carries `yolo` and nothing else, so "never ask" is legible and the
 * other two are not distinguishable from here. Returning `null` for that case
 * is the honest answer: a control that guessed "ask" for a session actually in
 * auto would tell someone permissions are coming that never will.
 */
export function permissionModeOf(yolo: boolean | undefined): PermissionMode | null {
  return yolo === true ? "always-approve" : null;
}

/** The `current_mode_update` broadcast's id, or `null` if this is not one. */
export function readModeUpdate(update: Record<string, unknown>): string | null {
  if (update["sessionUpdate"] !== "current_mode_update") return null;
  const id = update["currentModeId"];
  return typeof id === "string" ? id : null;
}
