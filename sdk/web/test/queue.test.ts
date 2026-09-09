// The queue's model, and the wire it is fed by.
//
// Two things are pinned here that a second implementation would get subtly
// wrong. The first is `editable`: it is the session's own answer and this
// client reads it rather than inferring one, so the tests state both the read
// and the fallback for an agent too old to answer. The second is the reorder
// shape — the handler pins protected rows to their absolute slots and reorders
// only the rest across what is left, so a swap is a swap between *mutable*
// neighbours, not between adjacent rows.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";

import type { SocketLike } from "../src/client.ts";
import { createGateway } from "../src/gateway.ts";
import { canMutate, createQueue, extraLines, firstLine, queueKind, reordered } from "../src/queue.ts";
import type { QueueChanged, QueueEntryWire, RosterEntry } from "../src/wire.ts";

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

function wire(over: Partial<QueueEntryWire> & { id: string }): QueueEntryWire {
  return { version: 0, kind: "prompt", text: "do the thing", ...over };
}

const QUEUE: QueueChanged = {
  sessionId: ENTRY.sessionId,
  entries: [
    wire({ id: "a", version: 3, text: "rename the parser\nand its tests" }),
    wire({ id: "b", kind: "parent_agent_message", text: "from the parent", editable: false }),
    wire({ id: "c", kind: "bash", text: "cargo test" }),
  ],
  runningPromptId: "r0",
  runningText: "rewrite the parser",
  runningKind: "prompt",
};

describe("what a queue row says about itself", () => {
  test("the row shows the first non-empty line and counts the rest", () => {
    // `QueuedPromptEntry::from_server`: the first non-empty line, trimmed, plus
    // a `(+N lines)` suffix counted over the whole text — so a prompt that
    // opens with a blank line still reports every line it has.
    expect(firstLine("\n\n  rename the parser  \nand its tests")).toBe("rename the parser");
    expect(extraLines("\n\n  rename the parser  \nand its tests")).toBe(3);
    expect(firstLine("   \n\t\n")).toBe("");
    expect(extraLines("")).toBe(0);
  });

  test("an unrecognised kind is a plain prompt, not a row this client refuses", () => {
    // `kind_from_wire` has the same tail. A kind nobody here has heard of is
    // still a queued prompt; whether it may be *touched* is a separate question
    // with a separate answer.
    expect(queueKind("bash")).toBe("bash");
    expect(queueKind("cron")).toBe("cron");
    expect(queueKind("command")).toBe("command");
    expect(queueKind("parent_agent_message")).toBe("prompt");
    expect(queueKind(undefined)).toBe("prompt");
  });

  test("mutability is read off the wire, and only guessed when nothing was said", () => {
    // The whole reason the field was added. A client inferring from `kind`
    // offers controls the session silently no-ops; the guess was right only
    // because exactly one origin is protected and its kind is named after it.
    expect(canMutate(wire({ id: "a", editable: false }))).toBe(false);
    expect(canMutate(wire({ id: "a", kind: "parent_agent_message", editable: true }))).toBe(true);
    expect(canMutate(wire({ id: "a", kind: "parent_agent_message" }))).toBe(false);
    expect(canMutate(wire({ id: "a" }))).toBe(true);
  });
});

describe("the queue as this client holds it", () => {
  test("nothing said is not an empty queue", () => {
    // An agent too old to answer `session/info`'s `queue` key leaves this
    // false forever, and a client that drew "nothing queued" from it would be
    // stating something nobody told it.
    const queue = createQueue();
    expect(queue.known).toBe(false);
    expect(queue.rows).toEqual([]);
    queue.apply({ sessionId: ENTRY.sessionId, entries: [] });
    expect(queue.known).toBe(true);
  });

  test("rows are numbered from one and carry the running turn beside them", () => {
    const queue = createQueue();
    queue.apply(QUEUE);
    expect(queue.rows.map((row) => [row.number, row.line])).toEqual([
      [1, "rename the parser"],
      [2, "from the parent"],
      [3, "cargo test"],
    ]);
    expect(queue.rows[0]!.hidden).toBe(1);
    expect(queue.rows[1]!.mutable).toBe(false);
    // The running turn is never one of the rows — it is drawn from the
    // transcript like any other turn — but it is named, because a send-now has
    // to be able to say which turn it would interrupt.
    expect(queue.rows.some((row) => row.id === "r0")).toBe(false);
    expect(queue.running?.line).toBe("rewrite the parser");
  });

  test("a broadcast with no running turn clears the one that was running", () => {
    const queue = createQueue();
    queue.apply(QUEUE);
    queue.apply({ sessionId: ENTRY.sessionId, entries: [] });
    expect(queue.running).toBeNull();
    expect(queue.rows).toEqual([]);
  });
});

describe("moving a row", () => {
  const rows = () => {
    const queue = createQueue();
    queue.apply(QUEUE);
    return queue.rows;
  };

  test("a swap steps over a protected row rather than pushing it", () => {
    // `handle_reorder_queue` pins protected and hidden rows to their absolute
    // slots and reorders only the queueable ones across what is left, so `c`
    // moving up trades places with `a` and `b` does not move.
    expect(reordered(rows(), "c", "up")).toEqual(["c", "b", "a"]);
    expect(reordered(rows(), "a", "down")).toEqual(["c", "b", "a"]);
  });

  test("nothing is sent for a row with nowhere to go, or one that may not move", () => {
    // An empty or unchanged order would still produce a rebroadcast, which is
    // exactly what a refusal looks like — so the two must not be confusable.
    expect(reordered(rows(), "a", "up")).toBeNull();
    expect(reordered(rows(), "c", "down")).toBeNull();
    expect(reordered(rows(), "b", "up")).toBeNull();
    expect(reordered(rows(), "nope", "up")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Over the wire
// ---------------------------------------------------------------------------

/** A leader that answers the handshake, one load and one info carrying a queue. */
class Leader implements SocketLike {
  sent: { method?: string; params?: Record<string, unknown> }[] = [];
  private listeners = new Map<string, ((event: never) => void)[]>();

  send(data: string): void {
    const frame = JSON.parse(data) as {
      id?: number;
      method?: string;
      params?: Record<string, unknown>;
    };
    if (frame.method === undefined) return;
    this.sent.push({ method: frame.method, params: frame.params });
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
      "_x.ai/session/plan": { result: { sessionId: ENTRY.sessionId } },
      "_x.ai/session/info": {
        result: { sessionId: ENTRY.sessionId, cwd: ENTRY.cwd, queue: QUEUE },
      },
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

  /** Push a notification at the client, the way the leader fans one out. */
  notify(method: string, params: unknown): void {
    this.receive(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }

  /** The params of the last `_x.ai/queue/<verb>` this client sent. */
  queueCall(verb: string): Record<string, unknown> | undefined {
    return this.sent.filter((frame) => frame.method === `_x.ai/queue/${verb}`).at(-1)?.params;
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

async function attached() {
  const gateway = createGateway();
  await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
  await gateway.attach(ENTRY);
  await settle();
  return gateway;
}

describe("where the queue comes from", () => {
  test("attaching brings the queue with the session, without a round trip of its own", async () => {
    // `x.ai/queue/changed` fires on a change and never on an attach, so a
    // client that only listened would draw an empty queue over prompts the
    // session is holding until somebody happened to touch one.
    const gateway = await attached();
    expect(gateway.attached()?.queue.rows.map((row) => row.id)).toEqual(["a", "b", "c"]);
    expect(leader.sent.filter((frame) => frame.method?.startsWith("_x.ai/queue")).length).toBe(0);
  });

  test("a broadcast for another session is not this session's queue", async () => {
    const gateway = await attached();
    leader.notify("x.ai/queue/changed", { sessionId: "somebody-else", entries: [] });
    expect(gateway.attached()?.queue.rows).toHaveLength(3);
    leader.notify("x.ai/queue/changed", { sessionId: ENTRY.sessionId, entries: [] });
    expect(gateway.attached()?.queue.rows).toHaveLength(0);
  });
});

describe("what a mutation puts on the wire", () => {
  test("each verb carries the session, the row, and the version where one is checked", async () => {
    const gateway = await attached();
    const rows = gateway.attached()!.queue.rows;

    gateway.queueRemove("a", rows[0]!.version);
    expect(leader.queueCall("remove")).toEqual({
      sessionId: ENTRY.sessionId,
      id: "a",
      expectedVersion: 3,
    });

    gateway.queueSendNow("a", rows[0]!.version);
    expect(leader.queueCall("interject")).toEqual({
      sessionId: ENTRY.sessionId,
      id: "a",
      expectedVersion: 3,
    });

    // Edit is last-write-wins rather than versioned: the handler takes an id
    // and the new text, bumps the version itself and records the editor
    // (`apply_queued_prompt_edit`).
    gateway.queueEdit("a", "rename it properly");
    expect(leader.queueCall("edit")).toEqual({
      sessionId: ENTRY.sessionId,
      id: "a",
      newText: "rename it properly",
    });

    gateway.queueMove("c", "up");
    expect(leader.queueCall("reorder")).toEqual({
      sessionId: ENTRY.sessionId,
      orderedIds: ["c", "b", "a"],
    });

    gateway.queueClear();
    expect(leader.queueCall("clear")).toEqual({ sessionId: ENTRY.sessionId });
  });

  test("no owner is sent, because sending one would scope every withdrawal to nothing", async () => {
    // The handlers match any row when no owner is given and only that client's
    // rows when one is. The pager sends none; this client sends no
    // `clientIdentifier` on `session/prompt` either, so its own rows are
    // unowned — an owner-scoped remove would match none of them.
    const gateway = await attached();
    gateway.queueRemove("a", 3);
    gateway.queueClear();
    for (const verb of ["remove", "clear"]) {
      expect(Object.keys(leader.queueCall(verb) ?? {})).not.toContain("owner");
      expect(Object.keys(leader.queueCall(verb) ?? {})).not.toContain("clientIdentifier");
    }
  });

  test("a move with nowhere to go sends nothing at all", async () => {
    const gateway = await attached();
    gateway.queueMove("a", "up");
    expect(leader.queueCall("reorder")).toBeUndefined();
  });

  test("nothing is applied locally; the broadcast is the only thing that moves the queue", async () => {
    // Every mutation is an ext-notification with no reply, and each handler
    // rebroadcasts even on a no-op — so a client that removed a row optimistically
    // would have shown a withdrawal the session refused.
    const gateway = await attached();
    gateway.queueRemove("a", 3);
    gateway.queueClear();
    await settle();
    expect(gateway.attached()?.queue.rows).toHaveLength(3);
  });
});
