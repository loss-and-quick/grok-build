import { describe, expect, test } from "bun:test";

import {
  ACTIVITY_ROLE,
  createRoster,
  directoryLabel,
  groupByDirectory,
  sessionLabel,
} from "../src/roster.ts";
import type { RosterActivity, RosterEntry } from "../src/wire.ts";

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
    expect(sessionLabel(entry({ sessionId: "sess-1", cwd: "/x" }))).toBe("sess-1");
    expect(sessionLabel(entry({ sessionId: "sess-1", cwd: "/x", title: "Fix it" }))).toBe("Fix it");
    expect(
      sessionLabel(entry({ sessionId: "sess-1", cwd: "/x", lastTurnSummary: "Fixed it" })),
    ).toBe("Fixed it");
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
