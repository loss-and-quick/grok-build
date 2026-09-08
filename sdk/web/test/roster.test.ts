import { describe, expect, test } from "bun:test";

import {
  ACTIVITY_ROLE,
  MAX_TITLE_CHARS,
  SHORT_ID_CHARS,
  createRoster,
  directoryLabel,
  displayTitle,
  groupByDirectory,
  sessionLabel,
} from "../src/roster.ts";
import type { RosterActivity, RosterEntry } from "../src/wire.ts";

const CRATES = new URL("../../../crates/codegen/", import.meta.url);
const TITLES = await Bun.file(new URL("xai-grok-pager/src/views/session_title.rs", CRATES)).text();
const FORBIDDEN = await Bun.file(
  new URL("xai-grok-shell/src/session/persistence.rs", CRATES),
).text();

function entry(over: Partial<RosterEntry> & { sessionId: string; cwd: string }): RosterEntry {
  return {
    isWorktree: false,
    yolo: false,
    activity: "idle",
    resident: true,
    lastChangeUnixMs: 0,
    origin: { kind: "local" },
    ...over,
  };
}

describe("roster grouped by working directory", () => {
  test("sessions land under their own cwd", () => {
    const groups = groupByDirectory([
      entry({ sessionId: "a", cwd: "/repo" }),
      entry({ sessionId: "b", cwd: "/other" }),
      entry({ sessionId: "c", cwd: "/repo" }),
    ]);
    expect(groups.map((g) => g.cwd).sort()).toEqual(["/other", "/repo"]);
    const repo = groups.find((g) => g.cwd === "/repo");
    expect(repo?.sessions.map((s) => s.sessionId).sort()).toEqual(["a", "c"]);
  });

  test("a worktree session is its own directory, not a child of the repo", () => {
    // The product's claim: an instance is a session with a fixed root. A
    // worktree session's root really is a different directory, and folding it
    // under the repo would be the client inventing a hierarchy the wire does
    // not describe.
    const groups = groupByDirectory([
      entry({ sessionId: "a", cwd: "/repo" }),
      entry({ sessionId: "b", cwd: "/home/me/.grok/worktrees/x", isWorktree: true }),
    ]);
    expect(groups).toHaveLength(2);
  });

  test("directories order by their most recent session, then by path", () => {
    const groups = groupByDirectory([
      entry({ sessionId: "old", cwd: "/a", lastChangeUnixMs: 10 }),
      entry({ sessionId: "new", cwd: "/b", lastChangeUnixMs: 99 }),
      entry({ sessionId: "mid", cwd: "/b", lastChangeUnixMs: 50 }),
    ]);
    expect(groups.map((g) => g.cwd)).toEqual(["/b", "/a"]);
    expect(groups[0]?.sessions.map((s) => s.sessionId)).toEqual(["new", "mid"]);
  });

  test("ties break on path so the order does not shuffle between renders", () => {
    const groups = groupByDirectory([
      entry({ sessionId: "a", cwd: "/z", lastChangeUnixMs: 1 }),
      entry({ sessionId: "b", cwd: "/a", lastChangeUnixMs: 1 }),
    ]);
    expect(groups.map((g) => g.cwd)).toEqual(["/a", "/z"]);
  });

  test("`sessions/changed` upserts and removes against the snapshot", () => {
    const roster = createRoster();
    roster.replace([entry({ sessionId: "a", cwd: "/repo" })]);
    roster.apply({
      upserted: [entry({ sessionId: "a", cwd: "/repo", activity: "working" })],
      removed: [],
    });
    expect(roster.get("a")?.activity).toBe("working");
    roster.apply({ upserted: [entry({ sessionId: "b", cwd: "/other" })], removed: ["a"] });
    expect(roster.get("a")).toBeUndefined();
    expect(roster.groups().map((g) => g.cwd)).toEqual(["/other"]);
  });

  test("a change payload with neither key is a no-op, not a crash", () => {
    const roster = createRoster();
    roster.replace([entry({ sessionId: "a", cwd: "/repo" })]);
    roster.apply({});
    expect(roster.all()).toHaveLength(1);
  });

  test("labels fall back the way the wire expects", () => {
    expect(directoryLabel("/home/me/grok-build")).toBe("grok-build");
    expect(directoryLabel("/home/me/grok-build/")).toBe("grok-build");
    expect(directoryLabel("/")).toBe("/");
    expect(sessionLabel(entry({ sessionId: "sess-1", cwd: "/x", title: "Fix it" }))).toBe("Fix it");
    expect(
      sessionLabel(entry({ sessionId: "sess-1", cwd: "/x", lastTurnSummary: "Fixed it" })),
    ).toBe("Fixed it");
  });
});

describe("what an untitled session is called", () => {
  test("the fallback chain is the one `entry_title` documents", () => {
    // The module doc lists the order in prose, so it is compared as prose: a
    // step added or reordered there stops matching here.
    expect(TITLES).toContain("1. `AgentView::display_name` if set (post-rename),");
    expect(TITLES).toContain("2. else `AgentView::generated_session_title`");
    expect(TITLES).toContain("first user-prompt block in scrollback,");
    expect(TITLES).toContain('4. else `"session abc12345"`');
  });

  test("a live untitled session is named by eight characters, not thirty-six", () => {
    // The defect: a session with no title yet fell through to the raw UUID, so
    // the page was headed by one. The terminal never shows that.
    const uuid = "3f2a1b9c-7d4e-4a1b-9c8d-0e1f2a3b4c5d";
    expect(sessionLabel(entry({ sessionId: uuid, cwd: "/x" }))).toBe("session 3f2a1b9c");
    expect(SHORT_ID_CHARS).toBe(8);
    expect(TITLES).toContain("sid.0.chars().take(8)");
  });

  test("the first prompt names a session before its id has to", () => {
    const uuid = "3f2a1b9c-7d4e-4a1b-9c8d-0e1f2a3b4c5d";
    expect(sessionLabel(entry({ sessionId: uuid, cwd: "/x" }), "  make the tests pass  ")).toBe(
      "make the tests pass",
    );
  });

  test("a title is cut where the pager cuts it", () => {
    expect(TITLES).toContain(`const MAX_TITLE_CHARS: usize = ${MAX_TITLE_CHARS};`);
    const long = "x".repeat(MAX_TITLE_CHARS + 5);
    expect(displayTitle(long)).toBe(`${"x".repeat(MAX_TITLE_CHARS)}...`);
    expect([...displayTitle(long)]).toHaveLength(MAX_TITLE_CHARS + 3);
    // Counted in characters, not bytes, so a multi-byte codepoint is not split.
    expect([...displayTitle("é".repeat(MAX_TITLE_CHARS + 2))]).toHaveLength(MAX_TITLE_CHARS + 3);
  });

  test("a control or a bidi override in a title becomes a replacement character", () => {
    // The same class the shell forbids. It is not decoration: a right-to-left
    // override in a heading can make a session read as a different one.
    expect(FORBIDDEN).toContain("c.is_control()");
    expect(FORBIDDEN).toContain("'\\u{200E}' | '\\u{200F}' | '\\u{202A}'..='\\u{202E}'");
    expect(displayTitle("a\u202Eb")).toBe("a\uFFFDb");
    expect(displayTitle("a\u0007b")).toBe("a\uFFFDb");
    expect(displayTitle("a\u2066b")).toBe("a\uFFFDb");
  });

  test("every activity the wire can send has a colour role", () => {
    const activities: RosterActivity[] = [
      "working",
      "idle",
      "needs_input",
      "dormant",
      "completed",
      "dead",
    ];
    expect(Object.keys(ACTIVITY_ROLE).sort()).toEqual([...activities].sort());
  });
});
