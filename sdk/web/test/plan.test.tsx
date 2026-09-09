// The saved plan: where it comes from, when it is asked for, and the one thing
// this client must not do while one is waiting to be approved.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";

import type { SocketLike } from "../src/client.ts";
import { PlanView } from "../src/components/PlanView.tsx";
import { createGateway, type Gateway } from "../src/gateway.ts";
import type { RosterEntry, SessionPlanResponse } from "../src/wire.ts";

const ENTRY: RosterEntry = {
  sessionId: "s1",
  cwd: "/home/me/repo",
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

const PLAN: SessionPlanResponse = {
  sessionId: ENTRY.sessionId,
  content: "# Rename the parser\n\n- move `parse` into its own module\n- keep the old name working",
  path: "/home/me/.grok/sessions/%2Fhome%2Fme%2Frepo/s1/plan.md",
  awaitingApproval: false,
};

function view(plan: SessionPlanResponse | null) {
  const gateway = { plan: () => plan } as unknown as Gateway;
  const { container } = render(() => PlanView({ gateway, onClose: () => {} }));
  return container;
}

describe("the plan, drawn", () => {
  test("the body is markdown, built as nodes rather than as a string of HTML", () => {
    // The pager opens the same file with `open_markdown_content`. Here the
    // parser hands back nodes and the renderer builds DOM from them, which is
    // what keeps "the plan is data, not markup" a property of the construction
    // rather than of a filter someone has to remember to apply.
    const container = view(PLAN);
    expect(container.querySelector(".plan-body h1")?.textContent).toBe("Rename the parser");
    expect(container.querySelectorAll(".plan-body li")).toHaveLength(2);
    expect(container.querySelector(".plan-body code")?.textContent).toBe("parse");
  });

  test("a plan carrying markup shows the markup as text", () => {
    const container = view({ ...PLAN, content: "<img src=x onerror=boom>" });
    expect(container.querySelectorAll(".plan-body img")).toHaveLength(0);
    expect(container.textContent).toContain("<img src=x onerror=boom>");
  });

  test("the file is named, because it is the one part a client cannot work out", () => {
    // `grok_home` joined with the URL-encoded cwd and the session id, on the
    // agent's machine rather than on this one.
    expect(view(PLAN).querySelector(".plan-path")?.textContent).toBe(PLAN.path ?? "");
  });

  test("a parked approval is said first, and this client does not offer to answer it", () => {
    // The approval is a reverse-request with an `outcome` this client has no
    // surface for. Saying where it can be answered is the honest form of that;
    // a button would not be one.
    const container = view({ ...PLAN, awaitingApproval: true });
    expect(container.querySelector(".plan-awaiting")?.textContent).toContain("terminal");
    expect(container.querySelector(".plan-body")).not.toBeNull();
  });

  test("an approval parked over an empty plan still says the agent has stopped", () => {
    // `content: null` with the flag set is the approved-but-empty case, and it
    // is the one where an inert "nothing written yet" would be actively wrong.
    const container = view({ sessionId: ENTRY.sessionId, awaitingApproval: true });
    expect(container.querySelector(".plan-awaiting")).not.toBeNull();
    expect(container.querySelector(".plan-empty")?.textContent).toContain("asking to leave");
  });
});

// ---------------------------------------------------------------------------
// When the client asks
// ---------------------------------------------------------------------------

class Leader implements SocketLike {
  asked: string[] = [];
  /** Frames this client sent that answer something: an id and no method. */
  answers: { id?: number; error?: { code: number } }[] = [];
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string };
    if (frame.method === undefined) {
      if (frame.id !== undefined) this.answers.push(frame);
      return;
    }
    this.asked.push(frame.method);
    if (frame.id === undefined) return;
    const results: Record<string, unknown> = {
      initialize: {
        protocolVersion: 1,
        authMethods: [{ id: "xai.api_key", name: "api key" }],
        _meta: { restoredAuthMeta: {} },
      },
      "session/load": {},
      "session/prompt": { stopReason: "end_turn" },
      "_x.ai/sessions/list": { result: { sessions: [ENTRY] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      "_x.ai/session/info": { result: { sessionId: ENTRY.sessionId, cwd: ENTRY.cwd } },
      "_x.ai/session/plan": { result: PLAN },
    };
    const result = results[frame.method];
    if (result === undefined) return;
    queueMicrotask(() => this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result })));
  }

  close(): void {}

  addEventListener(type: string, listener: (event: never) => void): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(listener);
    this.listeners.set(type, bucket);
    if (type === "open") queueMicrotask(() => (listener as () => void)());
  }

  receive(text: string): void {
    for (const listener of this.listeners.get("message") ?? []) {
      (listener as (event: { data: unknown }) => void)({ data: text });
    }
  }

  notify(method: string, params: unknown): void {
    this.receive(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }
}

let leader: Leader;
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  leader = new Leader();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return leader;
  };
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

const settle = (ms = 20): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));
const planCalls = (): number => leader.asked.filter((m) => m === "_x.ai/session/plan").length;

describe("when the plan is asked for", () => {
  test("on attaching, and at the end of a turn only while plan mode is on", async () => {
    // The pager's own cadence: `session/load` asks because a resumed session
    // may already have a plan written before this client existed, and
    // `turn_completion.rs` asks again only when `plan_mode_active` — every
    // other turn would be a round trip for a document nothing could have
    // written.
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
    await gateway.attach(ENTRY);
    await settle();
    expect(planCalls()).toBe(1);
    expect(gateway.plan()?.content).toBe(PLAN.content ?? "");

    await gateway.prompt("hello");
    expect(planCalls()).toBe(1);

    leader.notify("session/update", {
      sessionId: ENTRY.sessionId,
      update: { sessionUpdate: "current_mode_update", currentModeId: "plan" },
    });
    await gateway.prompt("plan it");
    expect(planCalls()).toBe(2);
  });

  test("an agent that has never heard of the method reads as no plan, not as an error", async () => {
    // `method not found` is what an older agent answers, and it is a
    // client-side "nothing to show" rather than something to report to a
    // person who can do nothing about it.
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
    leader.send = ((data: string) => {
      const frame = JSON.parse(data) as { id?: number; method?: string };
      if (frame.method === "_x.ai/session/plan" && frame.id !== undefined) {
        queueMicrotask(() =>
          leader.receive(
            JSON.stringify({
              jsonrpc: "2.0",
              id: frame.id,
              error: { code: -32601, message: "unsupported" },
            }),
          ),
        );
        return;
      }
      Leader.prototype.send.call(leader, data);
    }) as Leader["send"];
    await gateway.attach(ENTRY);
    await settle();
    expect(gateway.plan()).toEqual({});
  });
});
