// The shared prompt queue: what the session is holding, and what may be done to it.
//
// Pure and DOM-free like `tasks.ts` and `subagents.ts` beside it, and built on
// the same discipline — every field is either something the wire said or
// `undefined` because nothing said it.
//
// ## The queue is not a thing this client owns
//
// Every mutation is an **ext-notification**: no reply, no error, no result.
// `x.ai/queue/{edit,remove,reorder,clear,interject,hold_edit,release_edit}` are
// fire-and-forget, and the confirming `x.ai/queue/changed` broadcast is the
// entire feedback channel — the handlers rebroadcast even when they changed
// nothing, precisely so a refused mutation and an accepted one are told apart
// by what comes back rather than by silence.
//
// That is why nothing here is optimistic. A client that removed a row locally
// and then heard the row was protected would have shown a withdrawal that never
// happened; a client that waits shows a row that does not move, which is the
// truth. The terminal is optimistic in exactly one place — a remove it already
// knows is permitted — and pays for it with a reconciliation path this client
// does not need.
//
// ## `editable` is read, never inferred
//
// The session decides per row whether it will accept an edit, reorder, remove
// or send-now, then refuses anything that disagrees
// (`acp_session_impl/queue_mutation.rs`, `editable_queue_meta_matches`). That
// bit is on the wire now, so {@link canMutate} reads it. The kind-based guess
// below it is the fallback for an agent too old to say, and it is the rule the
// terminal used to apply unconditionally — right only because exactly one
// origin is protected and its kind is named after it.
import { createStore, reconcile } from "solid-js/store";

import type { QueueChanged, QueueEntryWire, QueueImageWire } from "./wire.ts";

/**
 * The one kind the session protects (`queue_pane.rs`, `from_wire_kind`).
 *
 * A message a parent agent queued into its child: the child's user may read it
 * and must not rewrite it.
 */
export const PROTECTED_KIND = "parent_agent_message";

/**
 * How a row is drawn, which is the only thing the wire `kind` decides here.
 *
 * `queue_pane.rs`'s `kind_from_wire`, including its "anything unrecognised is a
 * plain prompt" tail: a kind this client has never heard of is still a queued
 * prompt, and refusing to draw it would be worse than drawing it plainly.
 * Capability is a separate question and is answered by {@link canMutate}.
 */
export type QueueKind = "prompt" | "command" | "bash" | "cron";

export function queueKind(kind: string | undefined): QueueKind {
  switch (kind) {
    case "bash":
      return "bash";
    case "cron":
      return "cron";
    case "command":
      return "command";
    default:
      return "prompt";
  }
}

/**
 * Whether the session will accept a mutation of this row.
 *
 * One answer for all four verbs, because the session has one: `can_edit`,
 * `can_delete`, `can_reorder` and `can_send_now` are four readers of a single
 * `can_mutate` bit (`queue_pane.rs`, `ServerRowCapabilities`).
 */
export function canMutate(entry: QueueEntryWire): boolean {
  if (typeof entry.editable === "boolean") return entry.editable;
  return entry.kind !== PROTECTED_KIND;
}

/**
 * The line a row shows: the first non-empty one, trimmed.
 *
 * `QueuedPromptEntry::from_server` picks it exactly this way. A prompt that is
 * only whitespace shows as empty rather than as blank lines, which is what the
 * terminal draws too.
 */
export function firstLine(text: string): string {
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (trimmed !== "") return trimmed;
  }
  return "";
}

/**
 * How many lines the row is hiding behind that one.
 *
 * The terminal's `(+N lines)` suffix is `line_count - 1`, counted over the
 * whole text rather than from the shown line, so a prompt that opens with a
 * blank line still reports every line it has.
 */
export function extraLines(text: string): number {
  if (text === "") return 0;
  return Math.max(0, text.split("\n").length - 1);
}

/** One queued prompt, as this client draws and mutates it. */
export interface QueueRow {
  id: string;
  /** Sent back on every versioned mutation; a stale one is a silent no-op. */
  version: number;
  /** 1-based, the `#N` the terminal prints. */
  number: number;
  kind: QueueKind;
  /** The whole prompt, which is what an edit box opens on. */
  text: string;
  /** Its first non-empty line, which is what the collapsed row shows. */
  line: string;
  /** Lines the collapsed row is not showing. */
  hidden: number;
  /** Whether edit / reorder / withdraw / send-now may be offered. */
  mutable: boolean;
  /**
   * The images the session is holding for this row.
   *
   * Empty when the agent answered and this row has none; `undefined` when the
   * agent never answered at all, which is the case a row must not be described
   * as image-free in.
   */
  images?: readonly QueueImageWire[];
  /** The client that queued it, when it said. */
  owner?: string;
  /** The client that last edited it, when one has. */
  lastEditor?: string;
}

/** What the queue says about the turn currently running, if one is. */
export interface RunningPrompt {
  id: string;
  kind: QueueKind;
  text: string;
  line: string;
}

export interface Queue {
  /** Queued, not-yet-running prompts, in queue order. */
  readonly rows: readonly QueueRow[];
  /**
   * Whether anything has said what is queued.
   *
   * `false` until the first snapshot or broadcast lands, and it is not the same
   * as an empty queue: an agent too old to answer `session/info`'s `queue` key
   * leaves this false forever, and a client that drew "nothing queued" from it
   * would be stating something nobody told it.
   */
  readonly known: boolean;
  /** The turn being drained, when one is; never one of {@link rows}. */
  readonly running: RunningPrompt | null;
  /** Adopt a `x.ai/queue/changed` payload, or `session/info`'s copy of one. */
  apply(changed: QueueChanged): void;
  /** Forget everything, for a session that is being left. */
  clear(): void;
}

/** Read one wire row into the shape this client draws. */
function rowOf(entry: QueueEntryWire, at: number): QueueRow {
  const text = entry.text ?? "";
  return {
    id: entry.id,
    version: entry.version ?? 0,
    number: at + 1,
    kind: queueKind(entry.kind),
    text,
    line: firstLine(text),
    hidden: extraLines(text),
    mutable: canMutate(entry),
    images: entry.images ?? undefined,
    owner: entry.owner ?? undefined,
    lastEditor: entry.lastEditor ?? undefined,
  };
}

/**
 * The running turn as the queue describes it.
 *
 * Carried on the broadcast rather than read off the transcript because the
 * running row is deliberately absent from `entries`: a send-now has to be able
 * to name the turn it would interrupt, and a client keeping its own mirror of
 * that would be keeping a second copy of a thing the agent restates every time
 * it changes.
 */
function runningOf(changed: QueueChanged): RunningPrompt | null {
  const id = changed.runningPromptId;
  if (typeof id !== "string" || id === "") return null;
  const text = changed.runningText ?? "";
  return { id, kind: queueKind(changed.runningKind ?? undefined), text, line: firstLine(text) };
}

export function createQueue(): Queue {
  // `reconcile` rather than a plain replace: every mutation rebroadcasts the
  // whole queue, including the mutations that changed nothing, so a naive
  // replace would rebuild every row's DOM several times per keystroke while a
  // person is reordering. Keyed by `id`, which the session guarantees stable
  // across an edit — the version moves, the id does not.
  const [rows, setRows] = createStore<QueueRow[]>([]);
  const [state, setState] = createStore<{ known: boolean; running: RunningPrompt | null }>({
    known: false,
    running: null,
  });

  return {
    get rows() {
      return rows;
    },
    get known() {
      return state.known;
    },
    get running() {
      return state.running;
    },
    apply(changed: QueueChanged): void {
      setRows(reconcile((changed.entries ?? []).map(rowOf), { key: "id" }));
      setState({ known: true, running: runningOf(changed) });
    },
    clear(): void {
      setRows(reconcile([], { key: "id" }));
      setState({ known: false, running: null });
    },
  };
}

/**
 * The id order a swap asks for, or `null` when the row cannot move that way.
 *
 * A port of `server_queue_reordered` (`agent_view/queue.rs`), and the shape is
 * the handler's rather than a convenience: `handle_reorder_queue` pins every
 * protected row to its absolute slot and reorders only the mutable ones across
 * what is left. So a swap is a swap between *mutable neighbours* — a protected
 * row between them is stepped over, not pushed — and the list sent back names
 * every queued row so nothing is left to the "unnamed rows keep their relative
 * order" tail.
 *
 * `null` for a row at the end it is being moved towards, for a row that may not
 * be reordered, and for a row with no mutable neighbour that way. A caller that
 * turned that into an empty list would ask the agent to reorder nothing and get
 * a rebroadcast that looks exactly like a refusal.
 */
export function reordered(
  rows: readonly QueueRow[],
  id: string,
  direction: "up" | "down",
): string[] | null {
  const movable = rows.filter((row) => row.mutable).map((row) => row.id);
  const at = movable.indexOf(id);
  if (at < 0) return null;
  const to = direction === "up" ? at - 1 : at + 1;
  if (to < 0 || to >= movable.length) return null;
  const swapped = [...movable];
  const held = swapped[at]!;
  swapped[at] = swapped[to]!;
  swapped[to] = held;
  // Back into the slots they came from, so the protected rows keep the
  // positions the handler is going to keep them in anyway. Sending the mutable
  // ids alone would work — the handler ignores where a protected row appears —
  // but it would describe an order that is not the one on screen.
  let next = 0;
  return rows.map((row) => (row.mutable ? swapped[next++]! : row.id));
}
