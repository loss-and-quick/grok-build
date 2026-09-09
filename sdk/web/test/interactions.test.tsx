// A question the agent asked every attached client at once.
//
// The leader broadcasts a handful of reverse-requests to *every* subscriber of
// a session, with one id, and acts on the first answer (`leader/server.rs`,
// `is_interaction_request`). This client draws one of them and has no surface
// for the rest — and for those, "method not found" is not a statement about
// this client, it is the answer, sent on everyone's behalf, in a millisecond,
// while the person who can actually answer is still reading the card.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";

import type { SocketLike } from "../src/client.ts";
import { createGateway } from "../src/gateway.ts";
import type { RosterEntry } from "../src/wire.ts";

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

/** A leader that answers the handshake and one load, and records what comes back. */
class Leader implements SocketLike {
  answers: { id?: number; error?: { code: number }; result?: unknown }[] = [];
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string };
    if (frame.method === undefined) {
      if (frame.id !== undefined) this.answers.push(frame);
      return;
    }
    if (frame.id === undefined) return;
    const results: Record<string, unknown> = {
      initialize: {
        protocolVersion: 1,
        authMethods: [{ id: "xai.api_key", name: "api key" }],
        _meta: { restoredAuthMeta: {} },
      },
      "session/load": {},
      "_x.ai/sessions/list": { result: { sessions: [ENTRY] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      "_x.ai/session/info": { result: { sessionId: ENTRY.sessionId, cwd: ENTRY.cwd } },
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

/** Push a reverse-request at the attached client and see what it sends back. */
async function ask(method: string, params: unknown) {
  const gateway = createGateway();
  await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
  await gateway.attach(ENTRY);
  await settle();
  leader.answers = [];
  leader.receive(JSON.stringify({ jsonrpc: "2.0", id: 7, method, params }));
  await settle();
  return leader.answers;
}

describe("a question this client cannot draw", () => {
  test("a shared interaction goes unanswered rather than refused", async () => {
    // `x.ai/exit_plan_mode` is the sharpest case: the agent reads any error
    // other than an undeliverable one as "the client disconnected
    // mid-approval", abandons the tool call and cancels the turn
    // (`acp_session_impl/tool_calls.rs`). A browser answering instantly would
    // break plan-mode approval for the terminal beside it.
    expect(
      await ask("_x.ai/exit_plan_mode", {
        sessionId: ENTRY.sessionId,
        toolCallId: "tc1",
        planContent: "# Plan",
      }),
    ).toEqual([]);
    expect(await ask("_x.ai/ask_user_question", { sessionId: ENTRY.sessionId })).toEqual([]);
    expect(await ask("_x.ai/mcp/elicit", { sessionId: ENTRY.sessionId })).toEqual([]);
  });

  test("and anything else is still refused, because nobody else was asked", async () => {
    // Silence on a request routed to one client parks the turn until it times
    // out. The refusal is what keeps that from happening for everything this
    // client has no surface for.
    const answers = await ask("_x.ai/something/else", {});
    expect(answers).toHaveLength(1);
    expect(answers[0]?.error?.code).toBe(-32601);
  });

  test("one this client does draw is held open, not refused and not dropped", async () => {
    // Folder trust is deliberately *not* a shared interaction — it is routed to
    // the session's driver, once, and never replayed — so a client that treated
    // it like one would leave a workspace's configuration off with nobody else
    // to ask. Nothing comes back yet because the card is up: this request is
    // answered by a person, and the request stays open until they answer.
    const gateway = createGateway();
    await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
    await gateway.attach(ENTRY);
    await settle();
    leader.answers = [];
    leader.receive(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 9,
        method: "_x.ai/folder_trust/request",
        params: {
          sessionId: ENTRY.sessionId,
          cwd: ENTRY.cwd,
          workspace: ENTRY.cwd,
          configKinds: ["hooks"],
        },
      }),
    );
    await settle();
    expect(leader.answers).toEqual([]);
    expect(gateway.folderTrusts).toHaveLength(1);
  });
});
