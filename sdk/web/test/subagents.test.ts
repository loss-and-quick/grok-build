import { describe, expect, test } from "bun:test";

import {
  DURATION_BREAKS,
  MAX_ACTIVITY_SUBJECT_CHARS,
  PENDING_KILL_TIMEOUT_MS,
  activityOf,
  clampSubject,
  contextBadge,
  createSubagents,
  elapsedMs,
  formatDuration,
  metaSuffix,
  parseTagPrefix,
  sortRows,
  subagentLabel,
  typeLabel,
  type Subagent,
} from "../src/subagents.ts";
import type { SessionUpdate } from "../src/wire.ts";

const CRATES = new URL("../../../crates/codegen/", import.meta.url);
const rust = (path: string): Promise<string> => Bun.file(new URL(path, CRATES)).text();

const PARENT = "sess-parent";

function spawned(over: Record<string, unknown> = {}): SessionUpdate {
  return {
    sessionUpdate: "subagent_spawned",
    subagent_id: "sa-1",
    parent_session_id: PARENT,
    child_session_id: "sess-child-1",
    subagent_type: "general-purpose",
    description: "read alpha.txt",
    ...over,
  } as unknown as SessionUpdate;
}

function progress(over: Record<string, unknown> = {}): SessionUpdate {
  return {
    sessionUpdate: "subagent_progress",
    subagent_id: "sa-1",
    parent_session_id: PARENT,
    child_session_id: "sess-child-1",
    duration_ms: 4200,
    turn_count: 2,
    tool_call_count: 7,
    tokens_used: 30_000,
    context_window_tokens: 256_000,
    context_usage_pct: 12,
    tools_used: ["bash", "read_file"],
    error_count: 1,
    ...over,
  } as unknown as SessionUpdate;
}

function finished(over: Record<string, unknown> = {}): SessionUpdate {
  return {
    sessionUpdate: "subagent_finished",
    subagent_id: "sa-1",
    child_session_id: "sess-child-1",
    status: "completed",
    tool_calls: 9,
    turns: 3,
    duration_ms: 12_000,
    tokens_used: 41_000,
    output: "alpha",
    ...over,
  } as unknown as SessionUpdate;
}

describe("folding a fan-out off the parent's own stream", () => {
  test("a spawn is a row, and nothing it did not say is filled in", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());

    const row = subagents.rows[0]!;
    expect(row.subagentId).toBe("sa-1");
    expect(row.childSessionId).toBe("sess-child-1");
    expect(row.status).toBe("running");
    // The whole point: a spawn carries no counters, so there are none. A zero
    // here would be this client inventing "it has made no tool calls" out of
    // "the agent has not said".
    expect(row.turns).toBeUndefined();
    expect(row.toolCalls).toBeUndefined();
    expect(row.tokensUsed).toBeUndefined();
    expect(row.contextUsagePct).toBeUndefined();
    expect(row.durationMs).toBeUndefined();
    expect(elapsedMs(row, 1_000)).toBeUndefined();
  });

  test("a progress tick fills the counters, and the elapsed time keeps moving between ticks", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());
    subagents.apply(PARENT, progress());

    const row = subagents.rows[0]!;
    expect(row.turns).toBe(2);
    expect(row.toolCalls).toBe(7);
    expect(row.toolsUsed).toEqual(["bash", "read_file"]);
    expect(row.errorCount).toBe(1);
    // `duration_ms` is the agent's measurement; the wait since that frame
    // arrived is the client's own clock. Both are measured, so carrying the
    // number forward is arithmetic rather than a guess.
    const carried = elapsedMs(row, row.durationReadAtMs! + 1_000)!;
    expect(carried).toBeGreaterThanOrEqual(5_200);
    expect(carried).toBeLessThan(5_400);
  });

  test("a finish is terminal, and a second one does not re-finalize it", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());
    subagents.apply(PARENT, finished());
    subagents.apply(PARENT, finished({ status: "failed", error: "boom" }));

    const row = subagents.rows[0]!;
    expect(row.status).toBe("completed");
    expect(row.output).toBe("alpha");
    expect(row.error).toBeUndefined();
    expect(row.durationMs).toBe(12_000);
    // The agent's own final duration, not a clock that keeps running.
    expect(elapsedMs(row, 9e9)).toBe(12_000);
  });

  test("a duplicate spawn does not replace live state", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());
    subagents.apply(PARENT, progress());
    subagents.apply(PARENT, spawned({ description: "something else" }));

    expect(subagents.rows).toHaveLength(1);
    expect(subagents.rows[0]!.description).toBe("read alpha.txt");
    expect(subagents.rows[0]!.turns).toBe(2);
  });

  test("a grandchild is announced on its own parent's id, and nests under it", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());
    // The leader subscribes this client to the child at spawn, so the child's
    // own `subagent_spawned` arrives here without anything being asked for.
    subagents.apply(
      "sess-child-1",
      spawned({
        subagent_id: "sa-2",
        parent_session_id: "sess-child-1",
        child_session_id: "sess-grandchild",
      }),
    );

    expect(subagents.rows.map((row) => row.depth)).toEqual([0, 1]);
    expect([...subagents.childSessions()].sort()).toEqual(["sess-child-1", "sess-grandchild"]);
  });

  test("a spawn on a session this fan-out does not contain is dropped", () => {
    // Another client's session on the same leader. Its broadcasts reach this
    // socket; showing them under this session would be a fabrication.
    const subagents = createSubagents(PARENT);
    subagents.apply("sess-someone-else", spawned({ subagent_id: "sa-9" }));
    expect(subagents.rows).toHaveLength(0);
  });

  test("a finished child stops being a session worth listening to", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());
    expect(subagents.childSessions().has("sess-child-1")).toBe(true);
    subagents.apply(PARENT, finished());
    // The leader drops the route on the same event (`prune_child_route`), so
    // keeping it here would only mean listening for frames that cannot come.
    expect(subagents.childSessions().has("sess-child-1")).toBe(false);
  });

  test("finalizing a row the agent will not report on keeps the counters it did report", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());
    subagents.apply(PARENT, progress());
    subagents.finalize("sa-1", "unknown");

    const row = subagents.rows[0]!;
    expect(row.status).toBe("unknown");
    expect(row.toolCalls).toBe(7);
    expect(row.durationMs).toBe(4200);
  });

  test("a stop mark clears itself, so a lost answer does not disable the button forever", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());
    subagents.markKillSent("sa-1", 1_000);
    expect(subagents.rows[0]!.pendingKill).toBe(true);

    subagents.expireKills(1_000 + PENDING_KILL_TIMEOUT_MS - 1);
    expect(subagents.rows[0]!.pendingKill).toBe(true);
    subagents.expireKills(1_000 + PENDING_KILL_TIMEOUT_MS);
    expect(subagents.rows[0]!.pendingKill).toBe(false);
  });

  test("seeding from list_running gives a mid-fan-out attach its counters at once", () => {
    const subagents = createSubagents(PARENT);
    subagents.seed(PARENT, [
      {
        subagentId: "sa-7",
        parentSessionId: PARENT,
        childSessionId: "sess-child-7",
        subagentType: "explore",
        description: "survey the crate",
        startedAtEpochMs: 1_700_000_000_000,
        durationMs: 9_000,
        turnCount: 1,
        toolCallCount: 4,
        tokensUsed: 12_000,
        contextWindowTokens: 256_000,
        contextUsagePct: 5,
        toolsUsed: ["grep"],
        errorCount: 0,
      },
    ]);

    const row = subagents.rows[0]!;
    expect(row.status).toBe("running");
    expect(row.toolCalls).toBe(4);
    expect(row.durationMs).toBe(9_000);
    // `errorCount` really is zero here, because the agent said so.
    expect(row.errorCount).toBe(0);
  });
});

describe("what a child is doing, from the child's own stream", () => {
  test("the vocabulary is the pager's, not this client's", async () => {
    // `format_activity_label` (`app/subagent.rs:891`) is where these strings
    // live. Three of its arms are readable off the update stream; the rest come
    // from state the agent reports on paths a browser is not on.
    const body = await rust("xai-grok-pager/src/app/subagent.rs");
    const fn = body.slice(body.indexOf("pub(crate) fn format_activity_label"));
    for (const word of ["Thinking", "Responding", "Running: ", "Running tool"]) {
      expect(fn).toContain(`"${word}`);
    }

    expect(activityOf({ sessionUpdate: "agent_thought_chunk" } as SessionUpdate)).toBe("Thinking");
    expect(activityOf({ sessionUpdate: "agent_message_chunk" } as SessionUpdate)).toBe("Responding");
    expect(
      activityOf({ sessionUpdate: "tool_call", title: "cargo build" } as unknown as SessionUpdate),
    ).toBe("Running: cargo build");
    expect(
      activityOf({ sessionUpdate: "tool_call", title: "" } as unknown as SessionUpdate),
    ).toBe("Running tool");
  });

  test("a completed tool call says nothing about what the child is doing now", () => {
    // Returning a label here would freeze the row on the last finished tool.
    expect(
      activityOf({
        sessionUpdate: "tool_call_update",
        title: "cargo build",
        status: "completed",
      } as unknown as SessionUpdate),
    ).toBeUndefined();
    expect(activityOf({ sessionUpdate: "plan" } as unknown as SessionUpdate)).toBeUndefined();
  });

  test("the tool title is clamped where the pager clamps it", async () => {
    const tracker = await rust("xai-grok-pager/src/acp/tracker.rs");
    const declared = /pub const MAX_ACTIVITY_SUBJECT_CHARS: usize = (\d+);/.exec(tracker);
    expect(Number(declared![1])).toBe(MAX_ACTIVITY_SUBJECT_CHARS);

    const long = "x".repeat(MAX_ACTIVITY_SUBJECT_CHARS + 5);
    expect(clampSubject(long)).toBe(`${"x".repeat(MAX_ACTIVITY_SUBJECT_CHARS)}…`);
    // First non-empty line, trimmed — `clamp_activity_subject`.
    expect(clampSubject("\n  cargo test  \nsecond line")).toBe("cargo test");
  });

  test("only a running child gets an activity label", () => {
    const subagents = createSubagents(PARENT);
    subagents.apply(PARENT, spawned());
    subagents.applyChild("sess-child-1", {
      sessionUpdate: "agent_thought_chunk",
    } as SessionUpdate);
    expect(subagents.rows[0]!.activity).toBe("Thinking");

    subagents.apply(PARENT, finished());
    // The child is gone; its last thought is not what it is doing now.
    expect(subagents.rows[0]!.activity).toBeUndefined();
  });
});

describe("the pager's own labelling", () => {
  const row = (over: Partial<Subagent>): Subagent =>
    ({
      subagentId: "sa",
      parentSessionId: PARENT,
      childSessionId: "child",
      subagentType: "general-purpose",
      description: "do the thing",
      contextNormalized: false,
      seq: 0,
      depth: 0,
      status: "running",
      pendingKill: false,
      ...over,
    }) as Subagent;

  test("`general-purpose` displays as `general`, and anything else as itself", async () => {
    const body = await rust("xai-grok-pager/src/app/subagent.rs");
    const fn = body.slice(body.indexOf("pub(crate) fn format_type_label"));
    expect(fn).toContain('"general-purpose" => "general"');
    expect(typeLabel("general-purpose")).toBe("general");
    expect(typeLabel("explore")).toBe("explore");
  });

  test("a leading [tag] is a label, and leaves the description either way", () => {
    expect(parseTagPrefix("[ship] land the branch")).toEqual({ tag: "ship", rest: "land the branch" });
    expect(parseTagPrefix("[] nothing")).toEqual({ rest: "[] nothing" });
    expect(parseTagPrefix("no tag here")).toEqual({ rest: "no tag here" });
  });

  test("the label prefers persona, then role, then type, then the tag", () => {
    expect(subagentLabel(row({ persona: "scribe", role: "writer" })).label).toBe("Scribe");
    expect(subagentLabel(row({ role: "writer" })).label).toBe("Writer");
    expect(subagentLabel(row({ subagentType: "explore" })).label).toBe("Explore");
    expect(subagentLabel(row({ description: "[ship] land it" }))).toEqual({
      label: "Ship",
      description: "land it",
    });
    expect(subagentLabel(row({})).label).toBe("General");
  });

  test("persona and role collapse when they name the same thing", () => {
    expect(metaSuffix(row({ persona: "Writer", role: "writer", model: "grok-4" }))).toBe(
      " (Writer · grok-4)",
    );
    expect(metaSuffix(row({ role: "writer", model: "grok-4" }))).toBe(" (writer · grok-4)");
    expect(metaSuffix(row({}))).toBe("");
  });

  test("only `resumed` and `forked` are context badges", () => {
    expect(contextBadge(row({ contextSource: "resumed" }))).toBe("resumed");
    expect(contextBadge(row({ contextSource: "new" }))).toBe("");
  });

  test("the compact duration format is the pager's, thresholds and all", async () => {
    const util = await rust("xai-grok-pager-render/src/util.rs");
    const fn = util.slice(
      util.indexOf("pub fn format_duration"),
      util.indexOf("pub fn format_time_ago"),
    );
    // The three comparisons in `format_duration`, in source order.
    const breaks = [...fn.matchAll(/<\s*(\d+)/g)].map((m) => Number(m[1]));
    expect(breaks).toEqual([
      DURATION_BREAKS.tenths,
      DURATION_BREAKS.minute,
      DURATION_BREAKS.hour,
    ]);

    expect(formatDuration(5_200)).toBe("5.2s");
    expect(formatDuration(32_000)).toBe("32s");
    expect(formatDuration(125_000)).toBe("2m5s");
    expect(formatDuration(3_720_000)).toBe("1h2m");
  });

  test("the stop timeout is the pager's `PENDING_KILL_TIMEOUT_SECS`", async () => {
    const agent = await rust("xai-grok-pager/src/app/agent.rs");
    const declared = /pub const PENDING_KILL_TIMEOUT_SECS: u64 = (\d+);/.exec(agent);
    expect(Number(declared![1]) * 1000).toBe(PENDING_KILL_TIMEOUT_MS);
  });

  test("the order is the tasks pane's: running first, then type, then newest", () => {
    const rows = [
      row({ subagentId: "a", subagentType: "explore", seq: 0 }),
      row({ subagentId: "b", subagentType: "explore", seq: 1 }),
      row({ subagentId: "c", subagentType: "plan", seq: 2 }),
      row({ subagentId: "d", subagentType: "explore", seq: 3, status: "completed" }),
    ];
    expect(sortRows(rows).map((entry) => entry.subagentId)).toEqual(["b", "a", "c", "d"]);
  });
});

describe("the wire tags this fold switches on", () => {
  test("are the three the shell serializes", async () => {
    // `SessionUpdate` is `rename_all = "snake_case"` on the tag, so the variant
    // names below are the tags. A renamed variant, or a fourth one, lands here
    // rather than as a fan-out that silently stops rendering.
    const notification = await rust("xai-grok-shell/src/extensions/notification.rs");
    const enumBody = notification.slice(notification.indexOf("pub enum SessionUpdate {"));
    for (const variant of ["SubagentSpawned", "SubagentProgress", "SubagentFinished"]) {
      expect(enumBody).toContain(`    ${variant} {`);
    }
    const source = await Bun.file(new URL("../src/subagents.ts", import.meta.url)).text();
    for (const tag of ["subagent_spawned", "subagent_progress", "subagent_finished"]) {
      expect(source).toContain(`"${tag}"`);
    }
  });
});
