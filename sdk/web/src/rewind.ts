// Going back to an earlier turn: the points the agent offers, and the one call
// that discards everything after one of them.
//
// Both halves were already on the wire and this client used neither.
// `x.ai/rewind/points` lists every prompt as a checkpoint and
// `x.ai/rewind/execute` performs the cut
// (`xai-grok-shell/src/extensions/rewind.rs:19-20`), and the terminal reaches
// them through `Effect::FetchRewindPoints` and `Effect::RewindExecute`
// (`pager/src/app/effects/mod.rs:4216`, `:4275`).
//
// ## The casing seam, which is not what it looks like
//
// Two extension methods that live next to each other disagree, and the
// disagreement is in the Rust rather than in anyone's transcription.
// `RewindPointsResponse`, `RewindPointInfo` and `RewindResponse` carry no
// `rename_all` at all (`shell/src/session/acp_types.rs:321`, `:326`, `:293`),
// so they go out **snake_case** — `rewind_points`, `prompt_index`,
// `has_file_changes`, `target_prompt_index`, `prompt_text`. Their neighbour
// `ForkSessionResponse` *is* `rename_all = "camelCase"`
// (`shell/src/session/fork.rs:37`). The request side is the other way round:
// the handler's own params type takes both spellings through `#[serde(alias)]`
// (`extensions/rewind.rs:25-36`), which is why the terminal can send camelCase
// into a snake_case field and never notice.
//
// So the reader below accepts both and the writer sends what the terminal
// sends. Reading one spelling only would have worked in a test and failed on a
// live agent, which is the class of bug the aliases hide.
//
// ## What the terminal decided, and is reproduced here
//
// - **Conversation only, and forced.** `rewind_execute_params` sends
//   `mode: "conversation_only"` and `force: true`, always
//   (`pager/src/app/effects/mod.rs:5123-5134`). The wire also has `All` and
//   `FilesOnly`, and the terminal offers neither: a rewind never touches a
//   file. `force: true` is not "skip the checks" — with `force: false` the
//   agent runs a *dry run* and answers `success: false` with the conflicts
//   (`acp_session_impl/rewind.rs:242-259`), so the committing call is the
//   forced one.
// - **Newest first.** The picker sorts descending by prompt index
//   (`app/dispatch/rewind.rs:566`, in `handle_rewind_points_loaded`).
// - **The preview is the label.** `created_at`, `num_file_snapshots` and
//   `has_file_changes` all arrive and the terminal draws none of them; a row is
//   the prompt's first non-blank line, truncated to 60 by the agent
//   (`acp_session_impl/rewind.rs:45-64`), or `(no preview)`.
// - **The target prompt comes back to the composer.** A successful
//   conversation rewind answers with `prompt_text`, the text of the prompt that
//   was rewound away, and the terminal puts it back in the prompt widget
//   (`app/dispatch/rewind.rs:409`, `dispatch_rewind_success`).

/** `x.ai/rewind/execute`'s `mode`, as the terminal always sends it. */
export const REWIND_MODE = "conversation_only";

/**
 * One checkpoint.
 *
 * Every prompt is one: the agent generates `0..current_prompt_index` and marks
 * which of them have file snapshots (`acp_session_impl/rewind.rs:24-25`), so
 * the list is dense and `promptIndex` is a position rather than an id.
 */
export interface RewindPoint {
  /** Rewinding to N restores the state from before prompt N ran. */
  promptIndex: number;
  /** RFC 3339, and empty when the prompt left no file snapshot behind. */
  createdAt: string;
  numFileSnapshots: number;
  /** Whether the checkpoint has files a `mode` other than this one could revert. */
  hasFileChanges: boolean;
  /** The prompt's first non-blank line, already truncated by the agent. */
  promptPreview: string | null;
}

/** What `x.ai/rewind/execute` answered. */
export interface RewindResult {
  success: boolean;
  targetPromptIndex: number;
  /** The rewound-away prompt's own text, for the composer. */
  promptText: string | null;
  error: string | null;
}

/** The label a row shows, which is the terminal's own fallback included. */
export const NO_PREVIEW = "(no preview)";

function field(row: Record<string, unknown>, snake: string, camel: string): unknown {
  const value = row[snake];
  return value === undefined ? row[camel] : value;
}

function text(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

/**
 * Read `x.ai/rewind/points`, newest first.
 *
 * A row with no usable `prompt_index` is dropped rather than defaulted to zero:
 * zero is a real target — "throw the whole conversation away" — and inventing
 * it from a malformed row would offer the most destructive rewind there is as
 * if the agent had.
 */
export function readRewindPoints(response: unknown): RewindPoint[] {
  if (typeof response !== "object" || response === null) return [];
  const envelope = response as Record<string, unknown>;
  const rows = field(envelope, "rewind_points", "rewindPoints");
  if (!Array.isArray(rows)) return [];
  const points: RewindPoint[] = [];
  for (const raw of rows) {
    if (typeof raw !== "object" || raw === null) continue;
    const row = raw as Record<string, unknown>;
    const index = field(row, "prompt_index", "promptIndex");
    if (typeof index !== "number" || !Number.isInteger(index) || index < 0) continue;
    points.push({
      promptIndex: index,
      createdAt: text(field(row, "created_at", "createdAt")) ?? "",
      numFileSnapshots: Number(field(row, "num_file_snapshots", "numFileSnapshots") ?? 0),
      hasFileChanges: field(row, "has_file_changes", "hasFileChanges") === true,
      promptPreview: text(field(row, "prompt_preview", "promptPreview")),
    });
  }
  points.sort((a, b) => b.promptIndex - a.promptIndex);
  return points;
}

/** Read `x.ai/rewind/execute`. */
export function readRewindResult(response: unknown): RewindResult {
  if (typeof response !== "object" || response === null) {
    return { success: false, targetPromptIndex: -1, promptText: null, error: "empty response" };
  }
  const row = response as Record<string, unknown>;
  const index = field(row, "target_prompt_index", "targetPromptIndex");
  return {
    success: row["success"] === true,
    targetPromptIndex: typeof index === "number" ? index : -1,
    promptText: text(field(row, "prompt_text", "promptText")),
    error: text(row["error"]),
  };
}

export function rewindPointsParams(sessionId: string): { sessionId: string } {
  return { sessionId };
}

/**
 * The execute params, field for field as `rewind_execute_params` builds them
 * (`pager/src/app/effects/mod.rs:5124-5134`).
 */
export function rewindExecuteParams(
  sessionId: string,
  targetPromptIndex: number,
): { sessionId: string; targetPromptIndex: number; force: boolean; mode: string } {
  return { sessionId, targetPromptIndex, force: true, mode: REWIND_MODE };
}

/** What a row says. */
export function rewindLabel(point: RewindPoint): string {
  return point.promptPreview ?? NO_PREVIEW;
}

/**
 * The question the terminal asks before it cuts, word for word
 * (`views/rewind.rs`, `RewindPhase::Confirm`).
 *
 * Its `this turn` fallback is kept for the same reason the `(no preview)` one
 * is: a prompt that was only an image has no first line, and naming it by index
 * would be naming it by something the picker does not show.
 */
export function rewindConfirmTitle(point: RewindPoint): string {
  return `Rewind conversation to “${point.promptPreview ?? "this turn"}”?`;
}

// ---------------------------------------------------------------------------
// The marker, which is how a client learns that somebody else rewound
// ---------------------------------------------------------------------------
//
// This used to reach nobody. The agent wrote the marker to `updates.jsonl`
// through `persist_xai_update_only`, a function whose whole job is to persist
// *without* sending, so the only client that knew a rewind had happened was the
// one that asked for it; everyone else went on drawing turns the session no
// longer had. It is now sent as well as persisted
// (`acp_session_impl/rewind.rs`), and it carries a `sessionId`, so it fans out
// under the same rule as every other turn delta and reaches nobody who is not
// watching this session.
//
// It cannot arrive as history. `filter_rewind_lines` drops markers along with
// the branch they cut (`session/storage/mod.rs:1602`), so a replay never
// contains one and no client can act on the same rewind twice.
//
// The fields are snake_case, and that is not the seam described above: the
// extension enum's `rename_all` applies to the *tag* only, and these two field
// names are snake_case in the Rust to begin with
// (`extensions/notification.rs:634`). Both spellings are read anyway, for the
// same reason the points are.

/** The `sessionUpdate` tag, as `wire_tags.rs:118` pins it. */
export const REWIND_MARKER_TAG = "rewind_marker";

/** A rewind that has already happened. */
export interface RewindMarker {
  /** The conversation now ends before this prompt. */
  targetPromptIndex: number;
  /** RFC 3339, when the rewind was performed. */
  createdAt: string;
}

/**
 * Read one marker, or `null` if it is not one.
 *
 * A marker with no usable index is refused rather than defaulted, for the
 * reason {@link readRewindPoints} refuses a point with none: zero is a real
 * target, and acting on an invented one would throw a whole conversation off
 * the screen on a malformed frame.
 */
export function readRewindMarker(update: unknown): RewindMarker | null {
  if (typeof update !== "object" || update === null) return null;
  const row = update as Record<string, unknown>;
  if (row["sessionUpdate"] !== REWIND_MARKER_TAG) return null;
  const index = field(row, "target_prompt_index", "targetPromptIndex");
  if (typeof index !== "number" || !Number.isInteger(index) || index < 0) return null;
  return {
    targetPromptIndex: index,
    createdAt: text(field(row, "created_at", "createdAt")) ?? "",
  };
}
