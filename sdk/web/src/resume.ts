// What this client has already seen of a session, so a reconnect can ask for
// the rest of it instead of the whole of it.
//
// The wire has carried the machinery for this since before this client existed
// and the client used none of it. Every notification the agent persists is
// stamped with `_meta.eventId` — `"{sessionId}-{counter}"`, the same id on the
// live emission and on the line written to `updates.jsonl`
// (`xai-grok-shell-base/src/util/event_id.rs:19-59`) — and `session/load` takes
// the last one a client applied as `_meta.cursor`
// (`xai-grok-shell/src/agent/mvp_agent/session_setup.rs:908`). The agent then
// replays only what follows it, and — this is the part that makes it usable —
// sends that tail **without** `_meta.isReplay`, because those events are not
// history to a client that never saw them
// (`agent/mvp_agent/replay.rs:83-84`, `:306-307`).
//
// ## Not every id a client sees is a line it can name
//
// This is the fact the whole module is shaped around, and it is measurable
// rather than argued. A streaming reply arrives as many `agent_message_chunk`
// notifications, each with its own `eventId`, and is persisted as **one** line
// carrying the **last** chunk's id. Read off a real session's log: twelve live
// chunks, ids 4 through 15, one line, `"chunkId":12`, `eventId … -15`. Ids 4 to
// 14 exist on the wire and nowhere on disk.
//
// So a client that drops in the middle of a reply holds a cursor the agent
// cannot resolve, and gets the whole conversation back instead of the rest of
// one sentence — which is precisely the case a reconnect cursor exists for.
//
// The answer is to send the last id that is certainly a line of its own: the
// last update **before** the run of chunks that is currently open. A run closes
// as soon as anything else arrives — a tool call, a hook, the end of the turn —
// so on a session that is not mid-sentence this is simply the last event, and
// on one that is, it is the event before the sentence started.
//
// That cursor is behind what is on screen, so the two are brought back into
// agreement: {@link Resumption.mark} says where the transcript stood when the
// cursor settled, the caller drops the rest, and the tail re-sends them —
// the open run arriving as the single merged line it was persisted as.
//
// ## The stale cursor is the whole safety argument
//
// A cursor is resolved by exact string match against the rewind-filtered log
// (`session/storage/replay.rs:504-520`). Three things make it fail to resolve,
// and all three land on one branch:
//
//   - the client has no cursor at all (a first attach);
//   - the line it names was rewound away, or the log was truncated or rotated;
//   - the tail after it contains a line with no `eventId`, which the agent
//     refuses to send as live rather than guess (`replay.rs:508-518`).
//
// In every one of them `mark_replay` becomes true (`replay.rs:520`) and the
// agent replays the **entire** transcript with `isReplay: true` on every frame.
// So the client never has to judge its own cursor before sending: it asks with
// what it has, and the answer says which of the two it got. That is why the
// rule here is "keep what is on screen until a replayed frame arrives, and
// throw it away the moment one does".
//
// A rewind is correct by construction under the same rule, which is worth
// stating because it looks like the dangerous case. A rewind truncates a
// *suffix* of the log, and this client's cursor is at or before the last event
// it drew. Either the cursor line survived the truncation — and then so did
// everything before it, and the tail is exactly the post-rewind remainder — or
// it did not, and the whole thing is replayed. There is no third outcome in
// which the screen keeps a turn the leader has dropped.
//
// ## Why there are two ceilings, and why they are rolled back
//
// Dedup by counter is a safety net rather than the mechanism: the leader
// already buffers live notifications that arrive during an in-flight load and
// drops the ones at or below what the replay sent (`leader/server.rs:2062-2078`,
// `:2163-2174`). The net is still worth having, and the pager keeps it
// (`pager/src/app/acp_handler/mod.rs:170-180`) — but it keeps *two*, and the
// reason is not symmetry: ACP lines ride the agent's ordered event pipeline
// while xAI lines are emitted straight to the gateway, so a fresh xAI id can
// overtake queued lower-id ACP chunks. One shared ceiling would then read those
// chunks as stale and drop live text (`pager/src/app/agent_view/mod.rs:861-880`).
//
// A resume rolls both ceilings back to the cursor, which is what makes the
// screen, the cursor and the ceilings one consistent snapshot rather than three
// things that happen to agree. It matters because the log is **not** written in
// id order — the same real session has `eventId … -19` on the line above
// `… -15` — so a tail is a run of *file positions*, not of counters, and it can
// legitimately re-send a line whose id is below the cursor's. Rolled back, such
// a line is dropped and the copy already on screen (which the mark kept) stands.
// Left where it was, the drop would depend on an invariant about arrival order
// instead of on a snapshot, and the failure would be a silently missing line.

import type { Mark } from "./transcript.ts";

/** The three carriers collapse to two streams; see the note above. */
export type Stream = "acp" | "xai";

/**
 * Which stream a notification arrived on.
 *
 * `session/update` is ACP. `x.ai/session/update` (the carrier a replay uses)
 * and `x.ai/session_notification` (the carrier a live xAI event uses) are two
 * spellings of one stream, which is why this is a partition of three names into
 * two and not a fork in the client.
 */
export function streamOf(method: string): Stream {
  return method === "session/update" ? "acp" : "xai";
}

/**
 * The update tags whose consecutive frames are persisted as a single line.
 *
 * Not a guess about buffering: it is what the log contains. Only these three
 * stream, so only these three can leave a client holding an id that was never
 * written. Everything else — a tool call, a hook, the end of a turn, a panel —
 * is one notification and one line, and can be named the moment it arrives.
 */
const RUN_TAGS = new Set(["agent_message_chunk", "agent_thought_chunk", "user_message_chunk"]);

/** The two `_meta` fields a session notification carries that decide this. */
export interface UpdateMeta {
  /**
   * `_meta.eventId`, or absent.
   *
   * Absent is a real and ordinary case rather than an old agent: the pending
   * and resolved markers for a blocking question are broadcast with no `_meta`
   * at all (`session/pending_interaction.rs:44-56`), because they are not
   * persisted and so have no line to name. Such an update always applies and
   * never moves the cursor.
   */
  eventId?: string;
  /** `_meta.isReplay`: history being re-sent, not the session living. */
  isReplay: boolean;
}

/** Read the two fields out of a notification's `_meta`. */
export function readUpdateMeta(meta: Record<string, unknown> | undefined): UpdateMeta {
  const eventId = meta?.["eventId"];
  return {
    eventId: typeof eventId === "string" && eventId !== "" ? eventId : undefined,
    isReplay: meta?.["isReplay"] === true,
  };
}

/**
 * The counter at the end of an `eventId`.
 *
 * Split from the *last* dash, because a session id contains dashes of its own
 * and the counter never does — the agent's own rule
 * (`session/storage/replay.rs:494-497`, `pager/src/acp/meta.rs:89-96`). `null`
 * for an id whose tail is not a number, which is not an error: such an id is
 * still a usable cursor, it just cannot be ordered against another one.
 */
export function eventSeq(eventId: string): number | null {
  const at = eventId.lastIndexOf("-");
  if (at < 0) return null;
  const tail = eventId.slice(at + 1);
  return /^\d+$/.test(tail) ? Number(tail) : null;
}

/**
 * What to do with one arriving update.
 *
 * `"rebuild"` is the only one that is not about this frame: it says the agent
 * answered a cursored load with a full replay, so everything on screen is about
 * to be re-sent and what is there now has to go first. The frame it comes with
 * is the first of the new history and is applied after the wipe.
 */
export type Verdict = "apply" | "duplicate" | "rebuild";

/** The start of a transcript, which is where a cursor that names nothing points. */
const NOWHERE: Mark = { entries: 0, text: 0 };

/** What a finished `session/load` turned out to have been. */
export interface Arrival {
  /** How many updates were applied between the request and its answer. */
  frames: number;
  /** Whether the agent fell back to replaying the whole transcript. */
  rebuilt: boolean;
}

export interface Resumption {
  /**
   * The cursor to send, or `null` when nothing addressable has been drawn yet.
   *
   * Deliberately not the last event seen — see the note on merged runs above.
   */
  cursor(): string | null;
  /**
   * Where the transcript stood when {@link cursor} settled.
   *
   * Everything past it will be sent again, so it has to go before the tail
   * arrives, or the open run is drawn twice: once in pieces and once whole.
   */
  mark(): Mark;
  /**
   * A `session/load` has just been sent. `cursor` is what went with it, and
   * `null` means none did — which is what makes a replayed frame ordinary
   * rather than a signal to start over.
   */
  loading(cursor: string | null): void;
  /** Its answer has arrived. */
  loaded(): Arrival;
  /** Judge one update, before it is folded in. */
  verdict(stream: Stream, tag: string, meta: UpdateMeta): Verdict;
  /** Record one update, after it has been folded in. */
  drew(tag: string, meta: UpdateMeta): void;
}

/**
 * @param drawn where the transcript stands right now. Read from it rather than
 * counted here, because the mark has to be the transcript's own idea of its
 * position at the instant the cursor settled — a second count kept alongside it
 * would be a thing that can disagree.
 */
export function createResumption(drawn: () => Mark): Resumption {
  let cursorId: string | null = null;
  let cursorSeq: number | null = null;
  let cursorMark: Mark = NOWHERE;
  // The run of same-tag chunks still arriving, which is the part of the
  // transcript the log does not yet name.
  let open: { tag: string; id: string | undefined; seq: number | null } | null = null;
  const ceiling: Record<Stream, number | null> = { acp: null, xai: null };
  // A cursored load is in flight, so the first replayed frame means the cursor
  // did not resolve. Cleared by the first one and by the load's answer.
  let resuming = false;
  let frames = 0;
  let rebuilt = false;

  /**
   * Move the cursor, forward only.
   *
   * A lower id arriving later is not a rewind of anything: the two streams are
   * not delivered in one id order, so an out-of-order lifecycle event would
   * otherwise pull the cursor backwards and make the next reconnect re-deliver
   * a tail this client has already drawn. The pager's rule, and its wording: an
   * id with no parseable counter still advances the cursor and leaves the known
   * counter standing, so later numeric ids stay gated
   * (`pager/src/app/agent_view/session.rs:49-63`).
   */
  const settle = (id: string | undefined, seq: number | null, mark: Mark): void => {
    if (id === undefined) return;
    if (seq !== null && cursorSeq !== null && seq <= cursorSeq) return;
    cursorId = id;
    cursorMark = mark;
    if (seq !== null) cursorSeq = seq;
  };

  const forget = (): void => {
    cursorId = null;
    cursorSeq = null;
    cursorMark = NOWHERE;
    open = null;
    ceiling.acp = null;
    ceiling.xai = null;
  };

  return {
    cursor: () => cursorId,
    mark: () => cursorMark,

    loading(cursor) {
      resuming = cursor !== null;
      frames = 0;
      rebuilt = false;
      if (cursor === null) return;
      // Back to the snapshot the cursor names. The caller has dropped every
      // entry past the mark, so a ceiling still standing where the live stream
      // left it would refuse to redraw them.
      ceiling.acp = cursorSeq;
      ceiling.xai = cursorSeq;
      open = null;
    },

    loaded() {
      // Cleared here whatever happened, and that is the point rather than
      // tidiness: a replayed frame arriving with no load in flight is a leader
      // fanning another client's replay out to this one, and it must not be
      // able to wipe a transcript. Dropping it instead would be the pager's
      // stricter rule (`acp_handler/session_notification.rs:151-192`), but that
      // rule needs a grace window for the replay that lands just after its own
      // response, and getting *that* wrong loses history with nothing on screen
      // to say so. Appending it is what this client already did.
      resuming = false;
      return { frames, rebuilt };
    },

    verdict(stream, tag, meta) {
      const seq = meta.eventId === undefined ? null : eventSeq(meta.eventId);
      if (meta.isReplay) {
        const starting = resuming;
        if (starting) {
          resuming = false;
          rebuilt = true;
          // The counters of the history about to arrive are the counters this
          // client has already recorded, so the ceilings go with the
          // transcript — kept, they would read the replay as duplicates and
          // leave the screen empty.
          forget();
        }
        frames += 1;
        return starting ? "rebuild" : "apply";
      }
      const seen = ceiling[stream];
      if (seq !== null && seen !== null && seq <= seen) return "duplicate";
      // This update is going to be applied and it is not part of the run that
      // is open, so that run is finished and the line it was written as can be
      // named. The mark is taken now, before this update adds anything.
      if (open !== null && !(RUN_TAGS.has(tag) && tag === open.tag)) {
        settle(open.id, open.seq, drawn());
        open = null;
      }
      if (seq !== null) ceiling[stream] = seen === null ? seq : Math.max(seen, seq);
      frames += 1;
      return "apply";
    },

    drew(tag, meta) {
      const seq = meta.eventId === undefined ? null : eventSeq(meta.eventId);
      if (!RUN_TAGS.has(tag)) {
        settle(meta.eventId, seq, drawn());
        open = null;
        return;
      }
      // A chunk of the run that is open extends it; the last id wins, because
      // that is the one the merged line will carry.
      if (open?.tag === tag) {
        if (meta.eventId !== undefined) {
          open.id = meta.eventId;
          open.seq = seq;
        }
        return;
      }
      open = { tag, id: meta.eventId, seq };
    },
  };
}
