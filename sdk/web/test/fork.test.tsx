// Fork, pinned against `fork_session_params` and the shape of the call it makes.
//
// Two things here are easy to get wrong in a way nothing would notice.
//
//   1. **The response is camelCase and its neighbours are not.**
//      `ForkSessionResponse` is `rename_all = "camelCase"`
//      (`shell/src/session/fork.rs:37`) while `RewindPointsResponse` and
//      `RewindResponse`, handled a file away, carry no rename at all. A reader
//      that assumed one house style for `x.ai/*` would be right about one of
//      them.
//   2. **The call does not start a session.** `fork_session` copies files and
//      returns (`shell/src/session/fork.rs:1-2`), so a client that forked and
//      stopped would leave a session nobody is in and no row on screen until
//      something asked the roster again.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { cleanup, render } from "@solidjs/testing-library";

import type { SocketLike } from "../src/client.ts";
import { Session } from "../src/components/Session.tsx";
import { forkParams, readForkedSessionId } from "../src/fork.ts";
import { createGateway, type Gateway } from "../src/gateway.ts";
import type { RosterEntry } from "../src/wire.ts";

const ENTRY: RosterEntry = {
  sessionId: "parent",
  cwd: "/home/me/repo",
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

/** The child, as `merge_roster` emits a session with no resident actor. */
const CHILD: RosterEntry = {
  ...ENTRY,
  sessionId: "child",
  activity: "dormant",
  resident: false,
  lastChangeUnixMs: 2,
};

class FakeSocket implements SocketLike {
  sent: Record<string, unknown>[] = [];
  /** Flipped by the fork, the way the leader's roster gains the child's row. */
  forked = false;
  fork: unknown = { newSessionId: "child", parentSessionId: "parent", newCwd: ENTRY.cwd };

  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string; params?: unknown };
    this.sent.push(frame as Record<string, unknown>);
    if (frame.id === undefined || frame.method === undefined) return;
    if (frame.method === "_x.ai/session/fork") this.forked = true;
    const results: Record<string, unknown> = {
      initialize: { protocolVersion: 1, _meta: { currentWorkingDirectory: ENTRY.cwd } },
      "_x.ai/sessions/list": {
        result: { sessions: this.forked ? [ENTRY, CHILD] : [ENTRY] },
      },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      "session/load": {},
      "_x.ai/session/fork": this.fork,
    };
    const result = results[frame.method];
    if (result !== undefined) {
      queueMicrotask(() => this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result })));
    }
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

  calls(method: string): Record<string, unknown>[] {
    return this.sent.filter((f) => f["method"] === method);
  }
}

let socket: FakeSocket;
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  cleanup();
  socket = new FakeSocket();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return socket;
  };
});

afterAll(() => {
  cleanup();
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

const settle = (ms = 20): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

async function attached(): Promise<Gateway> {
  const gateway = createGateway();
  await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
  await gateway.attach(ENTRY);
  await settle();
  return gateway;
}

describe("the params are the terminal's", () => {
  test("all three paths are the parent's cwd, and the kind is `fork`", () => {
    // `fork_session_params` sets `sourceCwd` from a disk lookup that falls back
    // to the parent's cwd (`app/session_startup.rs:62-83`); a browser has only
    // the roster row, so it sends the row.
    expect(forkParams(ENTRY)).toEqual({
      sourceSessionId: "parent",
      sourceCwd: "/home/me/repo",
      newCwd: "/home/me/repo",
      sessionKind: "fork",
    });
  });

  test("a worktree parent adds `sourceWorkspaceDir`, and only then", () => {
    // The terminal's own condition: the key is set from `parent_is_worktree`
    // and omitted otherwise, which is why it is absent above rather than null.
    expect(forkParams({ ...ENTRY, isWorktree: true })).toEqual({
      sourceSessionId: "parent",
      sourceCwd: "/home/me/repo",
      newCwd: "/home/me/repo",
      sessionKind: "fork",
      sourceWorkspaceDir: "/home/me/repo",
    });
  });

  test("`targetPromptIndex` is not sent, because no client sends it", () => {
    // The field exists on `ForkSessionRequest` and `copy_session_data`
    // implements it, and `fork_session_params` never sets it. Forking from an
    // earlier turn is a thing the agent can do and neither client offers; this
    // asserts the browser has not quietly started offering it.
    expect(Object.keys(forkParams(ENTRY))).not.toContain("targetPromptIndex");
  });
});

describe("reading the answer", () => {
  test("the id is camelCase, unlike its neighbours a file away", () => {
    expect(readForkedSessionId({ newSessionId: "child" })).toBe("child");
    expect(readForkedSessionId({ new_session_id: "child" })).toBeNull();
  });

  test("an answer carrying an error is not an answer", () => {
    // `fork_response_new_session_id` bails on a non-null `error` before it looks
    // at anything else (`app/session_startup.rs:129-141`).
    expect(readForkedSessionId({ newSessionId: "child", error: "no space left" })).toBeNull();
    expect(readForkedSessionId({ result: { newSessionId: "child" } })).toBe("child");
    expect(readForkedSessionId({})).toBeNull();
    expect(readForkedSessionId(null)).toBeNull();
  });
});

describe("forking from the browser", () => {
  test("one call, and the roster is re-read so the child has a row", async () => {
    // The fork writes files and returns; nothing has loaded the child, so it is
    // dormant. Without the re-read the route for it would show an empty screen,
    // because `SessionRoute` needs the row's `cwd` to attach.
    const gateway = await attached();
    expect(gateway.roster.get("child")).toBeUndefined();
    const forked = await gateway.fork();
    expect(forked).toBe("child");
    expect(socket.calls("_x.ai/session/fork")).toHaveLength(1);
    expect(socket.calls("_x.ai/session/fork")[0]!["params"]).toEqual({
      sourceSessionId: "parent",
      sourceCwd: "/home/me/repo",
      newCwd: "/home/me/repo",
      sessionKind: "fork",
    });
    expect(gateway.roster.get("child")?.activity).toBe("dormant");
  });

  test("forking does not attach: the session on screen is still the parent", async () => {
    // Two clients, two meanings for "go there". The terminal switches to a
    // placeholder agent the instant it dispatches; a browser's route is the
    // address of the attached session, so the move belongs to whoever owns the
    // router — and this asserts the gateway does not do it on its own.
    const gateway = await attached();
    await gateway.fork();
    expect(gateway.attached()?.entry.sessionId).toBe("parent");
    expect(socket.calls("session/load")).toHaveLength(1);
  });

  test("an answer with no id is a failure, and nothing is opened", async () => {
    const gateway = await attached();
    socket.fork = { parentSessionId: "parent" };
    const opened: string[] = [];
    const { container } = render(() =>
      Session({ gateway, rail: false, onForked: (id) => opened.push(id) }),
    );
    (container.querySelector(".session-fork") as HTMLButtonElement).click();
    await settle();
    expect(opened).toEqual([]);
    expect(gateway.status()).toContain("fork failed");
  });

  test("the button opens the child once the agent has named it", async () => {
    const gateway = await attached();
    const opened: string[] = [];
    const { container } = render(() =>
      Session({ gateway, rail: false, onForked: (id) => opened.push(id) }),
    );
    const button = container.querySelector<HTMLButtonElement>(".session-fork")!;
    // Not disabled mid-turn, which is the terminal's decision: `dispatch_fork`
    // refuses only a session that has no id yet.
    expect(button.disabled).toBe(false);
    button.click();
    await settle();
    expect(opened).toEqual(["child"]);
  });
});
