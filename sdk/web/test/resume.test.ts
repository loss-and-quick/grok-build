// The bookkeeping a reconnect runs on, on its own.
//
// Everything here is a rule taken from the agent or the pager rather than
// invented, so each test names the thing it is pinned to. The end-to-end
// behaviour these add up to is in `reconnect.test.tsx`; this file is the part
// that can be wrong without any socket being involved.
import { describe, expect, test } from "bun:test";

import {
  createResumption,
  eventSeq,
  readUpdateMeta,
  streamOf,
  type Resumption,
  type Stream,
} from "../src/resume.ts";
import type { Mark } from "../src/transcript.ts";

const live = (eventId?: string) => ({ eventId, isReplay: false });
const replayed = (eventId?: string) => ({ eventId, isReplay: true });

/** A tag whose consecutive frames the agent persists as one line. */
const CHUNK = "agent_message_chunk";
/** A tag that is one notification and one line. */
const ONE = "tool_call";

/**
 * A resumption with a stand-in transcript under it.
 *
 * `drawn` is what `transcript.mark()` would answer: the caller says how many
 * entries a frame it passed judgement on would have added, which is exactly the
 * contract the gateway keeps by reading the real transcript.
 */
function harness(): {
  resumption: Resumption;
  /** Judge and record one update, adding `entries` transcript entries. */
  take: (stream: Stream, tag: string, meta: ReturnType<typeof live>, entries?: number) => string;
} {
  let drawn: Mark = { entries: 0, text: 0 };
  const resumption = createResumption(() => drawn);
  return {
    resumption,
    take(stream, tag, meta, entries = 0) {
      const verdict = resumption.verdict(stream, tag, meta);
      if (verdict === "duplicate") return verdict;
      if (verdict === "rebuild") drawn = { entries: 0, text: 0 };
      drawn = { entries: drawn.entries + entries, text: 0 };
      resumption.drew(tag, meta);
      return verdict;
    },
  };
}

describe("the counter inside an event id", () => {
  test("is the part after the last dash, because a session id has dashes", () => {
    // `session/storage/replay.rs:494-497` splits from the right for exactly
    // this reason; a session id is a UUID and the counter never contains one.
    expect(eventSeq("019e0000-0000-7000-8000-000000000001-0001")).toBe(1);
    expect(eventSeq("sess-1-42")).toBe(42);
  });

  test("is absent when the tail is not a number, and that is not an error", () => {
    // The pager's own case (`acp/meta.rs:205`). Such an id is still a usable
    // cursor — the agent matches the whole string — it just cannot be ordered.
    expect(eventSeq("weird-id-zzz")).toBeNull();
    expect(eventSeq("nodashes")).toBeNull();
  });
});

describe("the two fields read off a notification", () => {
  test("an absent `_meta` is not a malformed one", () => {
    // Ordinary rather than exceptional: the pending and resolved markers for a
    // blocking question carry no `_meta` at all
    // (`session/pending_interaction.rs:44-56`).
    expect(readUpdateMeta(undefined)).toEqual({ eventId: undefined, isReplay: false });
  });

  test("an empty event id is the same as none", () => {
    expect(readUpdateMeta({ eventId: "" }).eventId).toBeUndefined();
  });

  test("`isReplay` is read strictly", () => {
    expect(readUpdateMeta({ isReplay: true }).isReplay).toBe(true);
    expect(readUpdateMeta({ isReplay: "yes" }).isReplay).toBe(false);
  });
});

describe("three carriers, two streams", () => {
  test("the xAI replay and live carriers are one stream", () => {
    // They are two spellings of the same file's other half; only the ACP
    // carrier is ordered independently of them.
    expect(streamOf("session/update")).toBe("acp");
    expect(streamOf("x.ai/session/update")).toBe("xai");
    expect(streamOf("x.ai/session_notification")).toBe("xai");
  });
});

describe("the cursor names a line, not an event", () => {
  test("starts absent, so a first attach asks for everything", () => {
    expect(createResumption(() => ({ entries: 0, text: 0 })).cursor()).toBeNull();
  });

  test("a lone update settles as soon as it arrives", () => {
    // One notification, one line: it can be named the moment it is drawn.
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-1"), 1);
    expect(rig.resumption.cursor()).toBe("sess-1-1");
    expect(rig.resumption.mark().entries).toBe(1);
  });

  test("a run of chunks does not, because the log has not written it yet", () => {
    // The fact this whole module is shaped around: twelve live chunks are
    // persisted as one line under the *last* chunk's id, so the ids in the
    // middle of a reply exist on the wire and nowhere on disk. A cursor naming
    // one of them cannot resolve, and the agent answers with a full replay.
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-1"), 1);
    rig.take("acp", CHUNK, live("sess-1-2"), 1);
    rig.take("acp", CHUNK, live("sess-1-3"));
    expect(rig.resumption.cursor()).toBe("sess-1-1");
    expect(rig.resumption.mark().entries).toBe(1);
  });

  test("and settles on the run's last id the moment anything else arrives", () => {
    // That id is the one the merged line carries, so it is the first moment the
    // reply becomes addressable.
    const rig = harness();
    rig.take("acp", CHUNK, live("sess-1-2"), 1);
    rig.take("acp", CHUNK, live("sess-1-3"));
    rig.take("xai", "turn_completed", live("sess-1-4"));
    expect(rig.resumption.cursor()).toBe("sess-1-4");
    // The mark still covers the reply: it was complete before the turn ended.
    expect(rig.resumption.mark().entries).toBe(1);
  });

  test("a run interrupted by a run of another kind settles too", () => {
    // Thinking and speaking are two runs and two lines.
    const rig = harness();
    rig.take("acp", "agent_thought_chunk", live("sess-1-2"), 1);
    rig.take("acp", "agent_thought_chunk", live("sess-1-3"));
    rig.take("acp", CHUNK, live("sess-1-4"), 1);
    expect(rig.resumption.cursor()).toBe("sess-1-3");
    expect(rig.resumption.mark().entries).toBe(1);
  });

  test("does not go backwards when a lower id arrives later", () => {
    // `pager/src/app/agent_view/session.rs:49-63`. The two streams are not
    // delivered in one id order, so a late lifecycle event would otherwise pull
    // the cursor back and make the next reconnect re-deliver a drawn tail.
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-9"), 1);
    rig.take("xai", "hook_execution", live("sess-1-3"));
    expect(rig.resumption.cursor()).toBe("sess-1-9");
  });

  test("an id with no counter still advances it and leaves the counter standing", () => {
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-7"), 1);
    rig.take("acp", ONE, live("sess-1-opaque"), 1);
    expect(rig.resumption.cursor()).toBe("sess-1-opaque");
    // The known counter is retained, so a numeric id below it is still gated.
    expect(rig.take("acp", ONE, live("sess-1-3"))).toBe("duplicate");
    expect(rig.take("acp", ONE, live("sess-1-9"))).toBe("apply");
  });

  test("an update with no id at all applies and moves nothing", () => {
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-4"), 1);
    expect(rig.take("xai", "pending_interaction", live(undefined))).toBe("apply");
    expect(rig.resumption.cursor()).toBe("sess-1-4");
  });
});

describe("the duplicate guard", () => {
  test("drops a live update at or below what that stream has drawn", () => {
    const rig = harness();
    expect(rig.take("acp", ONE, live("sess-1-5"))).toBe("apply");
    expect(rig.take("acp", ONE, live("sess-1-5"))).toBe("duplicate");
    expect(rig.take("acp", ONE, live("sess-1-4"))).toBe("duplicate");
    expect(rig.take("acp", ONE, live("sess-1-6"))).toBe("apply");
  });

  test("holds one ceiling per stream, so a fresh xAI id cannot strand ACP text", () => {
    // The reason the pager keeps two (`app/agent_view/mod.rs:861-880`): ACP
    // lines ride the agent's ordered event pipeline while xAI lines go straight
    // to the gateway, so an xAI id can overtake queued ACP chunks. One shared
    // ceiling would read those chunks as stale and drop live text.
    const rig = harness();
    expect(rig.take("xai", "hook_execution", live("sess-1-20"))).toBe("apply");
    expect(rig.take("acp", CHUNK, live("sess-1-11"), 1)).toBe("apply");
  });

  test("never drops a replayed update", () => {
    // A replay is the agent restating history it knows this client wants; the
    // ceilings are about live delivery, and the pager exempts replay from them
    // too (`acp_handler/mod.rs:170-176`).
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-8"), 1);
    rig.resumption.loading(null);
    expect(rig.take("acp", ONE, replayed("sess-1-2"), 1)).toBe("apply");
  });
});

describe("a resume rolls the ceilings back to the cursor", () => {
  test("so the line the cursor's tail re-sends can be drawn again", () => {
    // The run's merged line carries its last chunk's id, which is above the
    // cursor and below where the live stream left the ceiling. Without the
    // rollback the tail's copy of the reply would be read as already drawn and
    // the rewound transcript would be left with a hole where it had been.
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-1"), 1);
    rig.take("acp", CHUNK, live("sess-1-2"), 1);
    rig.take("acp", CHUNK, live("sess-1-3"));
    rig.resumption.loading(rig.resumption.cursor());
    expect(rig.take("acp", CHUNK, live("sess-1-3"), 1)).toBe("apply");
  });

  test("and a line below the cursor is still refused", () => {
    // The log is not written in id order — a real session has `… -19` on the
    // line above `… -15` — so a tail is a run of file positions and may include
    // a line this client already holds. The mark kept it; the ceiling refuses
    // the copy.
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-15"), 1);
    rig.take("xai", "hook_execution", live("sess-1-19"));
    rig.resumption.loading(rig.resumption.cursor());
    expect(rig.take("acp", ONE, live("sess-1-15"))).toBe("duplicate");
  });

  test("a load with no cursor leaves them where they are", () => {
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-5"), 1);
    rig.resumption.loading(null);
    expect(rig.take("acp", ONE, live("sess-1-5"))).toBe("duplicate");
  });
});

describe("a load that answers a cursor with a full replay", () => {
  test("says so once, on the first replayed frame, and then applies the rest", () => {
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-1"), 1);
    rig.resumption.loading("sess-1-1");
    expect(rig.take("acp", ONE, replayed("sess-1-1"), 1)).toBe("rebuild");
    expect(rig.take("acp", ONE, replayed("sess-1-2"), 1)).toBe("apply");
    expect(rig.resumption.loaded()).toEqual({ frames: 2, rebuilt: true });
  });

  test("clears the ceilings with the transcript", () => {
    // Kept, they would read the replayed history — whose counters this client
    // has already recorded — as duplicates and leave the screen empty.
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-5"), 1);
    rig.resumption.loading("sess-1-5");
    rig.take("acp", ONE, replayed("sess-1-1"), 1);
    expect(rig.take("acp", ONE, live("sess-1-2"), 1)).toBe("apply");
  });

  test("takes the mark back to nothing, so the next resume cannot rewind into it", () => {
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-5"), 1);
    rig.resumption.loading("sess-1-5");
    rig.take("acp", ONE, replayed("sess-1-1"), 1);
    expect(rig.resumption.mark().entries).toBe(1);
    expect(rig.resumption.cursor()).toBe("sess-1-1");
  });

  test("does not fire for a load that sent no cursor", () => {
    // A first attach has an empty transcript already; a replay into it is
    // ordinary and wiping would be a no-op dressed as a decision.
    const rig = harness();
    rig.resumption.loading(null);
    expect(rig.take("acp", ONE, replayed("sess-1-1"), 1)).toBe("apply");
    expect(rig.resumption.loaded().rebuilt).toBe(false);
  });

  test("cannot fire once the load has been answered", () => {
    // A replayed frame arriving with no load in flight is a leader fanning
    // another client's replay out to this one. It must not be able to take the
    // conversation off the screen.
    const rig = harness();
    rig.resumption.loading("sess-1-1");
    rig.resumption.loaded();
    expect(rig.take("acp", ONE, replayed("sess-1-9"), 1)).toBe("apply");
  });
});

describe("what a finished load reports", () => {
  test("counts only the frames of the load it belongs to", () => {
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-1"), 1);
    rig.resumption.loading("sess-1-1");
    rig.take("acp", ONE, live("sess-1-2"), 1);
    rig.take("acp", ONE, live("sess-1-3"), 1);
    expect(rig.resumption.loaded()).toEqual({ frames: 2, rebuilt: false });
  });

  test("counts nothing when the cursor was current", () => {
    // The ordinary reconnect: the agent found the cursor and had nothing after
    // it. Zero frames is the measurement that "only what was missed" is true.
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-1"), 1);
    rig.resumption.loading("sess-1-1");
    expect(rig.resumption.loaded()).toEqual({ frames: 0, rebuilt: false });
  });

  test("a dropped duplicate is not a frame", () => {
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-4"), 1);
    rig.resumption.loading(null);
    rig.take("acp", ONE, live("sess-1-4"));
    expect(rig.resumption.loaded().frames).toBe(0);
  });
});

describe("the streams stay apart under a rebuild", () => {
  test("both ceilings go, not just the one the replay arrived on", () => {
    const streams: Stream[] = ["acp", "xai"];
    for (const stream of streams) {
      const rig = harness();
      rig.take("acp", ONE, live("sess-1-30"), 1);
      rig.take("xai", "hook_execution", live("sess-1-31"));
      rig.resumption.loading("sess-1-31");
      rig.take(stream, ONE, replayed("sess-1-1"), 1);
      expect(rig.take("acp", ONE, live("sess-1-2"), 1)).toBe("apply");
      expect(rig.take("xai", "hook_execution", live("sess-1-3"))).toBe("apply");
    }
  });
});

describe("an update the log will not hold", () => {
  test("does not end the run it arrives in the middle of", () => {
    // Measured against a live agent before it was fixed. A reply was streaming,
    // an id-less notification landed between two of its chunks, and the run was
    // declared over: the cursor settled onto a chunk from the middle of the
    // reply, the agent could not resolve it, and the whole conversation came
    // back. An update with no `eventId` is not a line in the log, so it is not a
    // boundary between lines either.
    const rig = harness();
    rig.take("xai", "turn_completed", live("sess-1-294"));
    rig.take("acp", "user_message_chunk", live("sess-1-295"), 1);
    rig.take("acp", CHUNK, live("sess-1-297"), 1);
    rig.take("acp", CHUNK, live("sess-1-298"));
    rig.take("xai", "pending_interaction", live(undefined));
    rig.take("acp", CHUNK, live("sess-1-299"));
    expect(rig.resumption.cursor()).toBe("sess-1-295");
  });

  test("and cannot be named as one either", () => {
    const rig = harness();
    rig.take("acp", ONE, live("sess-1-5"), 1);
    rig.take("xai", "pending_interaction", live(undefined));
    expect(rig.resumption.cursor()).toBe("sess-1-5");
  });
});
