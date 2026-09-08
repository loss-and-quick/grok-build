// A link that drops and comes back, from the gateway's own side.
//
// The socket is the whole subject, so these drive `createGateway` directly
// rather than the page: the sequence a reconnect actually performs is
// `connect` → `attach` → the socket dies → `connect` → `attach` with the same
// session id, and `createLink`'s ladder (tested in `link.test.ts`) only decides
// *when* the second `connect` happens. Everything that can be wrong about
// *what* is on the screen afterwards is in these two calls.
//
// The fake leader is deliberately faithful about two things a simpler one would
// get wrong, because both are load-bearing:
//
//   - every notification carries `_meta.eventId`, the same id the agent stamps
//     on the live emission and on the line it persists
//     (`xai-grok-shell-base/src/util/event_id.rs:19-59`);
//   - the replay a `session/load` performs arrives *before* its response, which
//     is what the agent guarantees by draining the replay first
//     (`agent/mvp_agent/replay.rs:243-249`) and what lets the client know the
//     count is final by the time it can act on it.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";

import type { SocketLike } from "../src/client.ts";
import { createGateway, type Gateway } from "../src/gateway.ts";
import type { MessageEntry } from "../src/transcript.ts";

const SESSION = "sess-1";
const CWD = "/home/me/repo";
const AGENT = "agent-a";

const ROSTER_ROW = {
  sessionId: SESSION,
  cwd: CWD,
  isWorktree: false,
  yolo: false,
  activity: "idle",
  resident: true,
  lastChangeUnixMs: 1,
  origin: { kind: "local" },
};

/** One `session/update`, as the leader puts it on a browser's socket. */
function update(
  sessionUpdate: string,
  text: string,
  meta: Record<string, unknown>,
  method = "session/update",
): string {
  return JSON.stringify({
    jsonrpc: "2.0",
    method,
    params: {
      sessionId: SESSION,
      update: { sessionUpdate, content: { type: "text", text } },
      _meta: meta,
    },
  });
}

/** The end of a turn, on the xAI carrier: one notification and one line. */
function ended(meta: Record<string, unknown>): string {
  return JSON.stringify({
    jsonrpc: "2.0",
    method: "_x.ai/session/update",
    params: { sessionId: SESSION, update: { sessionUpdate: "turn_completed" }, _meta: meta },
  });
}

/** What one `session/load` is answered with. */
interface Answer {
  /** Frames sent before the response, in order. */
  frames: string[];
}

class FakeLeader implements SocketLike {
  sent: string[] = [];
  /** The `_meta.cursor` of every `session/load` this socket received. */
  cursors: (string | undefined)[] = [];
  closed = false;
  /** Queued answers, one per load; the last one repeats. */
  answers: Answer[] = [{ frames: [] }];
  private listeners = new Map<string, ((event: never) => void)[]>();

  constructor(private readonly instanceId: string) {}

  send(data: string): void {
    this.sent.push(data);
    const frame = JSON.parse(data) as {
      id?: number;
      method?: string;
      params?: { _meta?: { cursor?: string } };
    };
    if (frame.id === undefined || frame.method === undefined) return;
    if (frame.method === "session/load") {
      this.cursors.push(frame.params?._meta?.cursor);
      const answer = this.answers.length > 1 ? this.answers.shift()! : this.answers[0]!;
      queueMicrotask(() => {
        for (const line of answer.frames) this.receive(line);
        this.reply(frame.id!, {});
      });
      return;
    }
    const results: Record<string, unknown> = {
      initialize: {
        protocolVersion: 1,
        authMethods: [{ id: "xai.api_key", name: "xai.api_key" }],
        _meta: {
          currentWorkingDirectory: CWD,
          restoredAuthMeta: {},
          agentId: AGENT,
          agentInstanceId: this.instanceId,
          hostname: "box",
          agentVersion: "1.0.0",
        },
      },
      "_x.ai/sessions/list": { result: { sessions: [ROSTER_ROW] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
    };
    // Everything else is answered empty rather than dropped. A request the fake
    // does not recognise is still a request the client is awaiting, and a leader
    // that never answers one is a hang, not a test.
    const result = results[frame.method] ?? {};
    queueMicrotask(() => this.reply(frame.id!, result));
  }

  private reply(id: number, result: unknown): void {
    this.receive(JSON.stringify({ jsonrpc: "2.0", id, result }));
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const listener of this.listeners.get("close") ?? []) (listener as () => void)();
  }

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

let sockets: FakeLeader[] = [];
let nextInstanceId = "";
const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  sockets = [];
  nextInstanceId = "run-1";
  localStorage.clear();
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    const socket = new FakeLeader(nextInstanceId);
    sockets.push(socket);
    return socket;
  };
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 10));

/** The socket the gateway is on right now. */
const leader = (): FakeLeader => sockets[sockets.length - 1]!;

async function connect(gateway: Gateway): Promise<void> {
  await gateway.connect("ws://127.0.0.1:2420/ws", "secret");
  await settle();
}

async function attach(gateway: Gateway): Promise<void> {
  const entry = gateway.roster.get(SESSION);
  if (!entry) throw new Error("the fake roster did not arrive");
  await gateway.attach(entry);
  await settle();
}

/** The socket dies where it stands, as a gateway being killed does. */
function drop(): void {
  leader().close();
}

const messages = (gateway: Gateway): MessageEntry[] =>
  gateway
    .attached()!
    .transcript.entries.filter((entry): entry is MessageEntry => entry.kind === "message");

const said = (gateway: Gateway, role: MessageEntry["role"]): string[] =>
  messages(gateway)
    .filter((entry) => entry.role === role)
    .map((entry) => entry.text);

/**
 * One finished turn, as the log holds it.
 *
 * The shape is copied from a real session's `updates.jsonl` rather than
 * invented: a streaming reply is persisted as **one** line under its last
 * chunk's id, and the turn's own end is a separate line after it. That last
 * line is what makes the reply addressable — see `resume.ts`.
 */
const FIRST_LOAD: Answer = {
  frames: [
    update("user_message_chunk", "ping", { isReplay: true, eventId: "sess-1-1" }),
    update("agent_message_chunk", "pong", { isReplay: true, eventId: "sess-1-2" }),
    ended({ isReplay: true, eventId: "sess-1-3" }),
  ],
};

describe("a socket that drops and comes back", () => {
  test("names the last line the agent wrote, not the last frame it sent", async () => {
    // The two are not the same while a reply is streaming, and that difference
    // is the whole of `resume.ts`: the chunks of a reply are persisted as one
    // line under the last chunk's id, so an id from the middle of one names
    // nothing on disk and the agent answers a full replay.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);
    const first = leader();
    // A reply beginning to stream. Its own id is unusable until the run ends.
    first.receive(update("agent_message_chunk", " and", { eventId: "sess-1-4" }));

    drop();
    await connect(gateway);
    await attach(gateway);

    expect(first.cursors).toEqual([undefined]);
    expect(leader().cursors).toEqual(["sess-1-3"]);
  });

  test("takes the half-written reply back and lets the tail send it whole", async () => {
    // The case the whole thing exists for: the turn was streaming when the link
    // died, the agent went on writing it, and what comes back is the *merged*
    // line — the reply entire. The half already on screen has to go first, or
    // the reader gets its first words twice.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);
    leader().receive(update("agent_message_chunk", " and", { eventId: "sess-1-4" }));
    expect(said(gateway, "assistant")).toEqual(["pong and"]);

    drop();
    await connect(gateway);
    // The tail the agent sends for a cursor it found: no `isReplay`, because to
    // this client these are not history (`agent/mvp_agent/replay.rs:306-307`).
    leader().answers = [
      { frames: [update("agent_message_chunk", " and again", { eventId: "sess-1-5" })] },
    ];
    await attach(gateway);

    expect(said(gateway, "user")).toEqual(["ping"]);
    expect(said(gateway, "assistant")).toEqual(["pong and again"]);
    expect(gateway.status()).toBe("resumed sess-1; caught up on 1 update");
  });

  test("says nothing was missed when the cursor was already current", async () => {
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    await connect(gateway);
    leader().answers = [{ frames: [] }];
    await attach(gateway);

    expect(said(gateway, "assistant")).toEqual(["pong"]);
    expect(gateway.status()).toBe("resumed sess-1; nothing was missed");
  });

  test("drops a live frame the tail had already delivered", async () => {
    // Belt to the leader's braces: it holds live notifications back while a
    // load is in flight and discards the ones the replay covered
    // (`leader/server.rs:2062-2078`). This is the half that does not depend on
    // which leader answered.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    await connect(gateway);
    leader().answers = [
      { frames: [update("agent_message_chunk", " again", { eventId: "sess-1-5" })] },
    ];
    await attach(gateway);
    leader().receive(update("agent_message_chunk", " again", { eventId: "sess-1-5" }));

    expect(said(gateway, "assistant")).toEqual(["pong again"]);
  });

  test("starts over when nothing it drew was ever written as a line", async () => {
    // A session whose only event so far is half of one message. There is no id
    // to resume after, so the carry-over is dropped rather than sent with a
    // cursor the agent would refuse — and the screen is rebuilt from the reply.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [
      { frames: [update("user_message_chunk", "pi", { isReplay: true, eventId: "sess-1-1" })] },
    ];
    await attach(gateway);

    drop();
    await connect(gateway);
    leader().answers = [
      { frames: [update("user_message_chunk", "ping", { isReplay: true, eventId: "sess-1-1" })] },
    ];
    await attach(gateway);

    expect(leader().cursors).toEqual([undefined]);
    expect(said(gateway, "user")).toEqual(["ping"]);
  });
});

describe("a cursor the agent cannot resolve", () => {
  test("rebuilds the conversation instead of doubling it", async () => {
    // The agent's fallback is a whole-transcript replay with `isReplay` on every
    // frame (`session/storage/replay.rs:520`), which is what a rewound or
    // rotated log produces. Folding it into what is already on screen is the
    // doubling this client has had before.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    await connect(gateway);
    leader().answers = [
      {
        frames: [
          update("user_message_chunk", "ping", { isReplay: true, eventId: "sess-1-1" }),
          update("agent_message_chunk", "pong done", { isReplay: true, eventId: "sess-1-2" }),
          ended({ isReplay: true, eventId: "sess-1-3" }),
        ],
      },
    ];
    await attach(gateway);

    expect(said(gateway, "user")).toEqual(["ping"]);
    expect(said(gateway, "assistant")).toEqual(["pong done"]);
  });

  test("tells the reader the conversation was reloaded, and only then", async () => {
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);
    // A resume that worked says nothing in the transcript.
    drop();
    await connect(gateway);
    leader().answers = [{ frames: [] }];
    await attach(gateway);
    expect(said(gateway, "notice")).toEqual([]);

    drop();
    await connect(gateway);
    leader().answers = [
      { frames: [update("agent_message_chunk", "pong", { isReplay: true, eventId: "sess-1-2" })] },
    ];
    await attach(gateway);

    expect(said(gateway, "notice")).toEqual([
      expect.stringContaining("reloaded from the agent") as unknown as string,
    ]);
    expect(gateway.status()).toBe("resumed sess-1; the conversation was reloaded");
  });
});

describe("a leader that restarted rather than a socket that blinked", () => {
  test("is told apart by `agentInstanceId` on the same `agentId`", async () => {
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    nextInstanceId = "run-2";
    await connect(gateway);
    leader().answers = [{ frames: [] }];
    await attach(gateway);

    expect(said(gateway, "notice")).toEqual([
      expect.stringContaining("agent restarted") as unknown as string,
    ]);
  });

  test("keeps the cursor across it, because the log outlived the process", async () => {
    // `agentInstanceId` says the running state is gone; it says nothing about
    // `updates.jsonl`, which is where a cursor is resolved. Dropping the cursor
    // here would buy a full replay and nothing else.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    nextInstanceId = "run-2";
    await connect(gateway);
    await attach(gateway);

    expect(leader().cursors).toEqual(["sess-1-3"]);
  });

  test("a socket that came back to the same process says nothing", async () => {
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    await connect(gateway);
    leader().answers = [{ frames: [] }];
    await attach(gateway);

    expect(said(gateway, "notice")).toEqual([]);
  });
});

describe("what is not carried over", () => {
  test("a different session on the same leader starts empty", async () => {
    // A carry-over describes the session the dead socket was showing. Attaching
    // to a different one must not inherit its cursor, or the agent would be
    // asked to resume session B after an event id that belongs to A.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    await connect(gateway);
    const other = { ...ROSTER_ROW, sessionId: "sess-2" } as never;
    leader().answers = [{ frames: [] }];
    await gateway.attach(other);
    await settle();

    expect(leader().cursors).toEqual([undefined]);
  });

  test("another machine on the same address starts empty", async () => {
    // Session ids are unique on a leader and not between leaders, so a matching
    // id behind a different `agentId` is a different conversation.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    (globalThis as { WebSocket: unknown }).WebSocket = function () {
      const socket = new FakeLeader("run-9");
      const original = socket.send.bind(socket);
      socket.send = (data: string) => {
        const frame = JSON.parse(data) as { method?: string; id?: number };
        if (frame.method !== "initialize") return original(data);
        socket.sent.push(data);
        queueMicrotask(() =>
          socket.receive(
            JSON.stringify({
              jsonrpc: "2.0",
              id: frame.id,
              result: {
                protocolVersion: 1,
                authMethods: [{ id: "xai.api_key", name: "xai.api_key" }],
                _meta: {
                  currentWorkingDirectory: CWD,
                  restoredAuthMeta: {},
                  agentId: "agent-b",
                  agentInstanceId: "run-9",
                },
              },
            }),
          ),
        );
      };
      sockets.push(socket);
      return socket;
    };
    await connect(gateway);
    leader().answers = [{ frames: [] }];
    await attach(gateway);

    expect(leader().cursors).toEqual([undefined]);
    expect(said(gateway, "notice")).toEqual([]);
  });
});

describe("what the gate must not swallow", () => {
  test("an update with no `_meta` still reaches the fold", async () => {
    // The pending and resolved markers for a blocking question are broadcast
    // with no `_meta` at all (`session/pending_interaction.rs:44-56`), so a gate
    // that required an event id would take every permission card off the screen.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);
    leader().receive(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "_x.ai/session_notification",
        params: {
          sessionId: SESSION,
          update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "!" } },
        },
      }),
    );

    expect(said(gateway, "assistant")).toEqual(["pong!"]);
  });

  test("a permission request still raises a card after a resume", async () => {
    // The leader caches an open interaction and replays it to a client that has
    // just attached (`leader/server.rs:2095-2114`), *after* the load response
    // and independently of the cursor — it is a reverse-request, not a line in
    // the log. A permission that vanished here would leave the agent parked
    // with nothing on screen.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);

    drop();
    await connect(gateway);
    leader().answers = [{ frames: [] }];
    await attach(gateway);
    leader().receive(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 900,
        method: "session/request_permission",
        params: {
          sessionId: SESSION,
          toolCall: { toolCallId: "call-1", title: "Run a command" },
          options: [{ optionId: "allow", name: "Allow", kind: "allow_once" }],
        },
      }),
    );
    await settle();

    expect(gateway.permissions.map((pending) => pending.toolCallId)).toEqual(["call-1"]);
  });

  test("and the copy the dead socket was showing is not left beside it", async () => {
    // Measured against a live agent before it was fixed: the card from the old
    // socket stayed, the leader re-sent the same request, and the same question
    // stood on screen twice. Pressing the older one writes the answer into a
    // socket with nowhere to send it, so the agent stays parked on a question
    // the person believes they have answered.
    const gateway = createGateway();
    await connect(gateway);
    leader().answers = [FIRST_LOAD];
    await attach(gateway);
    const asking = (id: number): string =>
      JSON.stringify({
        jsonrpc: "2.0",
        id,
        method: "session/request_permission",
        params: {
          sessionId: SESSION,
          toolCall: { toolCallId: "call-1", title: "Run a command" },
          options: [{ optionId: "allow", name: "Allow", kind: "allow_once" }],
        },
      });
    leader().receive(asking(900));
    await settle();
    expect(gateway.permissions.length).toBe(1);

    drop();
    await connect(gateway);
    leader().answers = [{ frames: [] }];
    await attach(gateway);
    leader().receive(asking(901));
    await settle();

    expect(gateway.permissions.map((pending) => pending.toolCallId)).toEqual(["call-1"]);
  });
});
