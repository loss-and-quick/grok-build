// The pane as it is actually driven: a real `Subagents` over a stub gateway,
// fed the frames the shell emits. What is asserted is mostly what the pane
// does NOT say — a counter the wire has not carried has to read as unknown,
// because a browser printing `0` there is inventing a fact about a child it
// cannot see.
import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";

import { Subagents } from "../src/components/Subagents.tsx";
import type { Gateway } from "../src/gateway.ts";
import { createSubagents, type Subagents as Store } from "../src/subagents.ts";
import { createTranscript } from "../src/transcript.ts";
import type { RosterEntry, SessionUpdate } from "../src/wire.ts";

const ENTRY: RosterEntry = {
  sessionId: "s1",
  cwd: "/home/me/repo",
  isWorktree: false,
  yolo: false,
  activity: "working",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

function stub(): { gateway: Gateway; store: Store; stopped: string[] } {
  const store = createSubagents(ENTRY.sessionId);
  const stopped: string[] = [];
  const gateway = {
    attached: () => ({ entry: ENTRY, transcript: createTranscript(), subagents: store }),
    cancelSubagent: async (subagentId: string) => {
      stopped.push(subagentId);
    },
  } as unknown as Gateway;
  return { gateway, store, stopped };
}

function spawn(store: Store, id: string, over: Record<string, unknown> = {}): void {
  store.apply(ENTRY.sessionId, {
    sessionUpdate: "subagent_spawned",
    subagent_id: id,
    parent_session_id: ENTRY.sessionId,
    child_session_id: `child-${id}`,
    subagent_type: "general-purpose",
    description: `task ${id}`,
    ...over,
  } as unknown as SessionUpdate);
}

function mount(gateway: Gateway) {
  const { container } = render(() => Subagents({ gateway }));
  const chips = (): string[] =>
    [...container.querySelectorAll(".subagent-chip")].map((n) => n.textContent ?? "");
  return {
    container,
    chips,
    title: () => container.querySelector(".subagents-title")?.textContent ?? "",
    names: () => [...container.querySelectorAll(".subagent-name")].map((n) => n.textContent ?? ""),
    stop: () => container.querySelector<HTMLButtonElement>(".stop-button"),
  };
}

describe("watching a fan-out in the browser", () => {
  test("nothing is drawn until a child exists", () => {
    const { gateway } = stub();
    const { container } = mount(gateway);
    expect(container.querySelector(".subagents")).toBeNull();
  });

  test("three children read as three rows, counted in the heading", () => {
    const { gateway, store } = stub();
    const view = mount(gateway);
    spawn(store, "a");
    spawn(store, "b");
    spawn(store, "c");

    expect(view.title()).toBe("Subagents — 3 running, 0 done");
    expect(view.names()).toHaveLength(3);
  });

  test("a child that has not ticked yet says so, rather than showing zeros", () => {
    const { gateway, store } = stub();
    const view = mount(gateway);
    spawn(store, "a");

    // Six chips, every one of them unknown: `subagent_spawned` carries no
    // counters and no start time at all.
    const chips = view.chips();
    expect(chips).toHaveLength(6);
    for (const chip of chips) expect(chip).toContain("—");
    expect(view.container.querySelector(".subagent-activity")?.textContent).toBe("unknown");
  });

  test("a progress tick replaces the unknowns with what the agent measured", () => {
    const { gateway, store } = stub();
    const view = mount(gateway);
    spawn(store, "a");
    store.apply(ENTRY.sessionId, {
      sessionUpdate: "subagent_progress",
      subagent_id: "a",
      parent_session_id: ENTRY.sessionId,
      child_session_id: "child-a",
      duration_ms: 32_000,
      turn_count: 2,
      tool_call_count: 7,
      tokens_used: 30_000,
      context_window_tokens: 256_000,
      context_usage_pct: 12,
      tools_used: ["bash"],
      error_count: 0,
    } as unknown as SessionUpdate);

    const chips = view.chips().join(" ");
    expect(chips).toContain("turns2");
    expect(chips).toContain("tools7");
    expect(chips).toContain("context12%");
    // Zero errors, because the agent said zero.
    expect(chips).toContain("errors0");
    expect(chips).toMatch(/elapsed3[23]s/);
  });

  test("the child's own stream is what says what it is doing", () => {
    const { gateway, store } = stub();
    const view = mount(gateway);
    spawn(store, "a");
    store.applyChild("child-a", {
      sessionUpdate: "tool_call",
      toolCallId: "t1",
      title: "cargo build --workspace",
      status: "in_progress",
    } as unknown as SessionUpdate);

    expect(view.container.querySelector(".subagent-activity")?.textContent).toBe(
      "Running: cargo build --workspace",
    );
  });

  test("finished rows are hidden until asked for, and then say how they ended", () => {
    const { gateway, store } = stub();
    const view = mount(gateway);
    spawn(store, "a");
    spawn(store, "b");
    store.apply(ENTRY.sessionId, {
      sessionUpdate: "subagent_finished",
      subagent_id: "b",
      child_session_id: "child-b",
      status: "failed",
      error: "the child gave up",
      tool_calls: 3,
      turns: 1,
      duration_ms: 4_000,
    } as unknown as SessionUpdate);

    expect(view.title()).toBe("Subagents — 1 running, 1 done");
    expect(view.names()).toHaveLength(1);

    view.container.querySelector<HTMLButtonElement>(".subagents-toggle")!.click();
    expect(view.names()).toHaveLength(2);
    expect(view.container.querySelector(".subagent-failed")).not.toBeNull();
    expect(view.container.querySelector(".subagent-error")?.textContent).toBe("the child gave up");
  });

  test("stopping a child takes two clicks, and the first one only arms it", () => {
    const { gateway, store, stopped } = stub();
    const view = mount(gateway);
    spawn(store, "a");

    const button = view.stop()!;
    expect(button.textContent).toContain("stop");
    button.click();
    // Armed, not sent. The terminal's single `x` lands on a row its cursor
    // already had to reach; a click in a list does not, and this is not undoable.
    expect(stopped).toEqual([]);
    expect(view.stop()!.textContent).toBe("confirm stop");

    view.stop()!.click();
    expect(stopped).toEqual(["a"]);
  });

  test("a stop in flight disables the button rather than offering itself again", () => {
    const { gateway, store } = stub();
    const view = mount(gateway);
    spawn(store, "a");
    store.markKillSent("a", performance.now());

    const button = view.stop()!;
    expect(button.disabled).toBe(true);
    expect(button.textContent).toBe("stopping…");
  });

  test("a grandchild is indented under the child that spawned it", () => {
    const { gateway, store } = stub();
    const view = mount(gateway);
    spawn(store, "a");
    store.apply("child-a", {
      sessionUpdate: "subagent_spawned",
      subagent_id: "b",
      parent_session_id: "child-a",
      child_session_id: "child-b",
      subagent_type: "explore",
      description: "look deeper",
    } as unknown as SessionUpdate);

    const depths = [...view.container.querySelectorAll<HTMLElement>(".subagent")].map((node) =>
      node.style.getPropertyValue("--depth"),
    );
    expect(depths.sort()).toEqual(["0", "1"]);
  });
});
