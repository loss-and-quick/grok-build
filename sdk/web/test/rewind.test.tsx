// Rewind, pinned against the agent's own rules rather than against this code.
//
// Three things are being held down here, and each of them is a way the browser
// could quietly disagree with the terminal.
//
//   1. **Casing.** The rewind responses go out snake_case and their neighbour
//      `session/fork` goes out camelCase, because the Rust says so
//      (`shell/src/session/acp_types.rs:321`, `:326`, `:293` against
//      `shell/src/session/fork.rs:37`). The request side takes both spellings
//      through `#[serde(alias)]`, which is exactly why reading one spelling
//      only would pass a test and fail on a live agent.
//   2. **The two calls.** `rewind_execute_params` sends `force: true` and
//      `mode: "conversation_only"`, always (`pager/src/app/effects/mod.rs:5124`).
//      `force: false` is not a safer rewind, it is a *dry run* that mutates
//      nothing and answers `success: false` (`acp_session_impl/rewind.rs:242`),
//      so a client that "played it safe" would silently never rewind at all.
//   3. **Who decides what is left.** The agent does. A rewind is followed by a
//      `session/load` on the cursor this client holds, and the reply either
//      resolves it or replays everything; computing the cut here instead would
//      be this client guessing at a truncation it cannot see.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { cleanup, render } from "@solidjs/testing-library";

import type { SocketLike } from "../src/client.ts";
import { RewindPicker } from "../src/components/RewindPicker.tsx";
import { Session } from "../src/components/Session.tsx";
import { createGateway, type Gateway } from "../src/gateway.ts";
import {
  NO_PREVIEW,
  readRewindMarker,
  readRewindPoints,
  readRewindResult,
  rewindConfirmTitle,
  rewindExecuteParams,
  rewindLabel,
} from "../src/rewind.ts";
import { createQueue } from "../src/queue.ts";
import { createSubagents } from "../src/subagents.ts";
import { createTranscript } from "../src/transcript.ts";
import type { RosterEntry, SessionUpdate } from "../src/wire.ts";

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

/** One replayed frame: the update, and the event id the log knows it by. */
interface Frame {
  update: SessionUpdate;
  event: number;
}

const user = (text: string, event: number): Frame => ({
  update: { sessionUpdate: "user_message_chunk", content: { type: "text", text } },
  event,
});
const agent = (text: string, event: number): Frame => ({
  update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } },
  event,
});
/**
 * A tool call, which is the frame that makes a cursor possible.
 *
 * Only the three chunk tags are merged into one persisted line, so only they
 * can leave a client holding an id the log never wrote (`resume.ts`). Anything
 * else closes the run and can be named — which is why a transcript of nothing
 * but messages has no cursor at all, and why that case gets its own test below.
 */
const tool = (id: string, event: number): Frame => ({
  update: { sessionUpdate: "tool_call", toolCallId: id, title: "ls", kind: "execute" },
  event,
});

/** The whole conversation, as the agent replays it on a first attach. */
const WHOLE: Frame[] = [
  user("first prompt", 1),
  agent("first answer", 2),
  tool("call-1", 3),
  user("second prompt", 4),
  agent("second answer", 5),
];

/** What is left after rewinding to prompt 1: the first turn, and nothing else. */
const AFTER: Frame[] = [user("first prompt", 1), agent("first answer", 2), tool("call-1", 3)];

class FakeSocket implements SocketLike {
  sent: Record<string, unknown>[] = [];
  /** What the next `session/load` replays, and whether it is marked history. */
  replay: Frame[] = WHOLE;
  /** What `x.ai/rewind/points` answers with. */
  points: unknown = {
    rewind_points: [
      {
        prompt_index: 0,
        created_at: "2026-09-09T10:00:00Z",
        num_file_snapshots: 2,
        has_file_changes: true,
        prompt_preview: "first prompt",
      },
      {
        prompt_index: 1,
        created_at: "",
        num_file_snapshots: 0,
        has_file_changes: false,
        prompt_preview: "second prompt",
      },
    ],
  };
  /** What `x.ai/rewind/execute` answers with. */
  execute: unknown = {
    success: true,
    target_prompt_index: 1,
    mode: "conversation_only",
    reverted_files: [],
    clean_files: [],
    conflicts: [],
    prompt_text: "second prompt",
    error: null,
  };

  /**
   * When set, `x.ai/rewind/execute` answers only on {@link release}.
   *
   * The agent broadcasts the marker *before* it answers the execute, so the one
   * window that matters for the initiator is between the two. Holding the
   * answer is the only way to open that window on purpose.
   */
  holdExecute = false;
  private held: (() => void)[] = [];

  private listeners = new Map<string, ((event: never) => void)[]>();

  /** Let a held `x.ai/rewind/execute` answer. */
  release(): void {
    const waiting = this.held;
    this.held = [];
    for (const answer of waiting) answer();
  }

  /**
   * The rewind broadcast, on the carrier the agent sends it on.
   *
   * `send_xai_notification` puts it on `x.ai/session_notification` with the
   * ordinary `_meta` every persisted update carries, so it arrives stamped with
   * an `eventId` like any other frame — which is exactly why the client has to
   * decide not to record it rather than never being offered the chance.
   */
  marker(target: number, event = 9, sessionId = ENTRY.sessionId): void {
    this.receive(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "_x.ai/session_notification",
        params: {
          sessionId,
          update: {
            sessionUpdate: "rewind_marker",
            target_prompt_index: target,
            created_at: "2026-09-09T10:05:00Z",
          },
          _meta: { eventId: `${sessionId}-${event}` },
        },
      }),
    );
  }

  send(data: string): void {
    const frame = JSON.parse(data) as { id?: number; method?: string; params?: unknown };
    this.sent.push(frame as Record<string, unknown>);
    if (frame.id === undefined || frame.method === undefined) return;
    // The replay is drained before the answer, which is what the agent does
    // (`agent/mvp_agent/replay.rs:243-249`) and what lets `resumption.loaded()`
    // report a final count.
    if (frame.method === "session/load") {
      for (const item of this.replay) {
        this.receive(
          JSON.stringify({
            jsonrpc: "2.0",
            method: "session/update",
            params: {
              sessionId: ENTRY.sessionId,
              update: item.update,
              _meta: { eventId: `${ENTRY.sessionId}-${item.event}`, isReplay: true },
            },
          }),
        );
      }
    }
    const results: Record<string, unknown> = {
      initialize: { protocolVersion: 1, _meta: { currentWorkingDirectory: ENTRY.cwd } },
      "_x.ai/sessions/list": { result: { sessions: [ENTRY] } },
      "_x.ai/settings/list": { catalog: { version: 1, categories: [], rows: [] }, state: {} },
      "_x.ai/subagent/list_running": { result: { subagents: [] } },
      "session/load": {},
      "_x.ai/rewind/points": this.points,
      "_x.ai/rewind/execute": this.execute,
    };
    const result = results[frame.method];
    if (result !== undefined) {
      const answer = (): void =>
        this.receive(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result }));
      if (this.holdExecute && frame.method === "_x.ai/rewind/execute") this.held.push(answer);
      else queueMicrotask(answer);
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

function mount(gateway: Gateway) {
  let rewound: string | null | undefined;
  let closed = false;
  const { container } = render(() =>
    RewindPicker({
      gateway,
      onClose: () => (closed = true),
      onRewound: (text) => (rewound = text),
    }),
  );
  return {
    container,
    rows: (): string[] =>
      [...container.querySelectorAll(".rewind-row-preview")].map((n) => n.textContent ?? ""),
    click: (at: number) =>
      (container.querySelectorAll<HTMLButtonElement>(".rewind-row")[at] as HTMLButtonElement).click(),
    question: (): string => container.querySelector(".rewind-question")?.textContent ?? "",
    warning: (): string => container.querySelector(".rewind-warning")?.textContent ?? "",
    go: () => (container.querySelector(".rewind-go") as HTMLButtonElement | null)?.click(),
    back: () => (container.querySelector(".rewind-back") as HTMLButtonElement | null)?.click(),
    failure: (): string => container.querySelector(".rewind-failed-message")?.textContent ?? "",
    state: (): string => container.querySelector(".rewind-state")?.textContent ?? "",
    escape: () =>
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })),
    rewound: () => rewound,
    closed: () => closed,
  };
}

const texts = (gateway: Gateway): string[] =>
  gateway
    .attached()!
    .transcript.entries.filter((e) => e.kind === "message")
    .map((e) => (e as { text: string }).text);

describe("reading the wire the agent actually writes", () => {
  test("the points come back snake_case, and newest first", () => {
    // Neither of these is cosmetic. The struct carries no `rename_all`
    // (`acp_types.rs:321`, `:326`), and the terminal's picker sorts descending
    // (`app/dispatch/rewind.rs:566`) — a list in the agent's own ascending
    // order would put "throw the whole conversation away" under the cursor.
    const points = readRewindPoints({
      rewind_points: [
        { prompt_index: 0, prompt_preview: "one", has_file_changes: true },
        { prompt_index: 2, prompt_preview: "three" },
        { prompt_index: 1, prompt_preview: "two" },
      ],
    });
    expect(points.map((p) => p.promptIndex)).toEqual([2, 1, 0]);
    expect(points.map((p) => p.promptPreview)).toEqual(["three", "two", "one"]);
    expect(points[2]!.hasFileChanges).toBe(true);
    expect(points[1]!.hasFileChanges).toBe(false);
  });

  test("camelCase is read too, because the request side accepts both", () => {
    // The handler's own params take either spelling (`extensions/rewind.rs:25`),
    // so an agent that answers in the other one is not a broken agent.
    const points = readRewindPoints({
      rewindPoints: [{ promptIndex: 4, promptPreview: "later", hasFileChanges: true }],
    });
    expect(points).toEqual([
      {
        promptIndex: 4,
        createdAt: "",
        numFileSnapshots: 0,
        hasFileChanges: true,
        promptPreview: "later",
      },
    ]);
  });

  test("a row with no usable index is dropped rather than defaulted to zero", () => {
    // Zero is a real target: it discards the whole conversation. Inventing it
    // from a malformed row would offer the most destructive rewind there is as
    // though the agent had.
    expect(readRewindPoints({ rewind_points: [{ prompt_preview: "no index" }] })).toEqual([]);
    expect(readRewindPoints({ rewind_points: [{ prompt_index: -1 }] })).toEqual([]);
    expect(readRewindPoints({ rewind_points: "not a list" })).toEqual([]);
    expect(readRewindPoints(null)).toEqual([]);
  });

  test("a point with no preview says so rather than showing an index", () => {
    const [point] = readRewindPoints({ rewind_points: [{ prompt_index: 3, prompt_preview: "" }] });
    expect(point!.promptPreview).toBeNull();
    expect(rewindLabel(point!)).toBe(NO_PREVIEW);
    // The terminal's own two fallbacks differ, and both are kept: a row says
    // `(no preview)` and the question says `this turn`.
    expect(rewindConfirmTitle(point!)).toBe("Rewind conversation to “this turn”?");
  });

  test("the result is read snake_case, and a refusal keeps its reason", () => {
    expect(
      readRewindResult({ success: true, target_prompt_index: 2, prompt_text: "redo me" }),
    ).toEqual({ success: true, targetPromptIndex: 2, promptText: "redo me", error: null });
    const refused = readRewindResult({
      success: false,
      target_prompt_index: 9,
      error: "Cannot rewind to prompt #9 — current prompt index is 2",
    });
    expect(refused.success).toBe(false);
    expect(refused.error).toContain("current prompt index is 2");
  });

  test("the execute params are the pager's, `force` included", () => {
    // `force: false` would be a dry run that changes nothing
    // (`acp_session_impl/rewind.rs:242`), so the committing call is the forced
    // one and "safer" here means "never rewinds".
    expect(rewindExecuteParams("s1", 3)).toEqual({
      sessionId: "s1",
      targetPromptIndex: 3,
      force: true,
      mode: "conversation_only",
    });
  });
});

describe("picking a turn", () => {
  test("the list is what the agent offered, newest first", async () => {
    const gateway = await attached();
    const view = mount(gateway);
    await settle();
    expect(view.rows()).toEqual(["second prompt", "first prompt"]);
  });

  test("clicking a row asks; it does not rewind", async () => {
    // The whole reason this dialog has a second step. The terminal's `Enter`
    // is already the second half of a gesture that began with walking a cursor
    // onto the row; a click has no first half, so the confirm supplies it —
    // the same trade the rail's stop button makes.
    const gateway = await attached();
    const view = mount(gateway);
    await settle();
    view.click(0);
    expect(socket.calls("_x.ai/rewind/execute")).toHaveLength(0);
    expect(view.question()).toBe("Rewind conversation to “second prompt”?");
    // And it says the two things the rows cannot. The reach half of that
    // sentence used to be a warning that the other clients would *not* see the
    // rewind, which was true only while the agent kept the marker to itself.
    expect(view.warning()).toContain("in every client attached to this session");
    expect(view.warning()).toContain("Files are left alone");
  });

  test("saying no goes back to the list, and Escape steps back before closing", async () => {
    const gateway = await attached();
    const view = mount(gateway);
    await settle();
    view.click(0);
    view.back();
    expect(view.question()).toBe("");
    expect(view.rows()).toHaveLength(2);

    view.click(1);
    expect(view.question()).toBe("Rewind conversation to “first prompt”?");
    view.escape();
    expect(view.question()).toBe("");
    expect(view.closed()).toBe(false);
    view.escape();
    expect(view.closed()).toBe(true);
  });

  test("a turn that edited files says so, because this rewind leaves files alone", async () => {
    // `has_file_changes` rides every point and the terminal draws none of it.
    // It is drawn here for one reason: the mode this client sends is
    // `conversation_only`, so a turn's edits survive the rewind of its turn.
    const gateway = await attached();
    const view = mount(gateway);
    await settle();
    const notes = [...view.container.querySelectorAll(".rewind-row-files")];
    expect(notes).toHaveLength(1);
    expect(view.rows()[1]).toBe("first prompt");
  });
});

describe("the agent decides what is left", () => {
  test("a rewind reloads without a cursor, and the replay replaces rather than doubles", async () => {
    const gateway = await attached();
    expect(texts(gateway)).toEqual([
      "first prompt",
      "first answer",
      "second prompt",
      "second answer",
    ]);
    // A cursor is held, and it is deliberately not used below.
    expect(gateway.attached()!.resumption.cursor()).toBe("s1-3");

    socket.replay = AFTER;
    const view = mount(gateway);
    await settle();
    view.click(0);
    view.go();
    await settle();

    const execute = socket.calls("_x.ai/rewind/execute");
    expect(execute).toHaveLength(1);
    expect(execute[0]!["params"]).toEqual({
      sessionId: "s1",
      targetPromptIndex: 1,
      force: true,
      mode: "conversation_only",
    });

    // No cursor, and this is the regression. Sending one was the first version,
    // and against a live agent it left the screen showing a turn the leader had
    // discarded: a cursor can resolve against a line that sits before the cut
    // in the file while the position recorded for it here is late, and then the
    // tail is empty and the mark restores what the agent has thrown away.
    const loads = socket.calls("session/load");
    expect(loads).toHaveLength(2);
    expect((loads[1]!["params"] as { _meta?: unknown })._meta).toBeUndefined();

    // Not five entries and not seven: exactly what the agent replayed.
    expect(texts(gateway)).toEqual(["first prompt", "first answer"]);
    expect(gateway.attached()!.transcript.entries).toHaveLength(3);
  });

  test("a rewind that leaves nothing leaves nothing on screen", async () => {
    // The case the cursor got wrong, in the shape it was found in: rewinding to
    // prompt 0 keeps no turn at all, and a client that trusted its cursor here
    // kept the whole conversation.
    const gateway = await attached();
    socket.replay = [];
    const view = mount(gateway);
    await settle();
    view.click(1);
    view.go();
    await settle();
    expect(gateway.attached()!.transcript.entries).toHaveLength(0);
  });

  test("the discarded prompt comes back for the composer", async () => {
    const gateway = await attached();
    socket.replay = AFTER;
    const view = mount(gateway);
    await settle();
    view.click(0);
    view.go();
    await settle();
    expect(view.rewound()).toBe("second prompt");
    expect(view.closed()).toBe(true);
  });

  test("the transcript is cleared before the replay, not appended to", async () => {
    // The case the `reset()` in `loadTranscript` exists for, and the reason a
    // cursorless load needs one: nothing is in flight for a replayed frame to
    // be recognised against, so the fold cannot tell it from live and every
    // surviving turn would be drawn a second time.
    //
    // Driven here through a session that has no cursor at all — only the three
    // chunk tags are merged into one persisted line, so a conversation of
    // nothing but messages never settles one — because that also pins the case
    // where the clear is the *only* thing standing between a reload and a
    // doubled screen.
    socket.replay = [user("only prompt", 1), agent("only answer", 2)];
    const gateway = await attached();
    expect(gateway.attached()!.resumption.cursor()).toBeNull();
    expect(texts(gateway)).toEqual(["only prompt", "only answer"]);

    socket.replay = [user("only prompt", 1)];
    const view = mount(gateway);
    await settle();
    view.click(0);
    view.go();
    await settle();

    const loads = socket.calls("session/load");
    expect((loads[1]!["params"] as { _meta?: unknown })._meta).toBeUndefined();
    expect(texts(gateway)).toEqual(["only prompt"]);
  });

  test("a refusal is shown in the agent's words and nothing is reloaded", async () => {
    const gateway = await attached();
    socket.execute = {
      success: false,
      target_prompt_index: 1,
      error: "Cannot rewind to prompt #1 — compaction checkpoint data is unavailable",
    };
    const view = mount(gateway);
    await settle();
    view.click(0);
    view.go();
    await settle();

    expect(view.failure()).toContain("compaction checkpoint data is unavailable");
    expect(view.closed()).toBe(false);
    expect(socket.calls("session/load")).toHaveLength(1);
    expect(texts(gateway)).toHaveLength(4);
  });
});

describe("the phase a browser cannot offer", () => {
  test("the button is off while a turn runs, and says why", () => {
    // The pager's `CancelOffer` phase offers to cancel the running turn and
    // then rewind (`views/rewind.rs`, `RewindPhase::CancelOffer`). Cancelling a
    // turn is not something this client can do, so the honest form of that
    // phase is not to open the picker at all.
    const transcript = createTranscript();
    const gateway = {
      attached: () => ({ entry: ENTRY, transcript, subagents: createSubagents(ENTRY.sessionId), queue: createQueue() }),
      permissions: [],
      folderTrusts: [],
      sessionMode: () => null,
      setSessionMode: async () => {},
      setPermissionMode: () => {},
      status: () => "running…",
      commands: () => [],
      models: () => null,
      prompt: async () => {},
      panelAction: async () => {},
    } as unknown as Gateway;
    const { container } = render(() => Session({ gateway, rail: false }));
    const button = container.querySelector<HTMLButtonElement>(".session-rewind")!;
    expect(button.disabled).toBe(true);
    expect(button.title).toContain("Wait for this turn to finish");
  });
});

// ---------------------------------------------------------------------------
// A rewind performed by somebody else
// ---------------------------------------------------------------------------
//
// The marker used to reach nobody: the agent persisted it through
// `persist_xai_update_only`, a function whose whole job is to write without
// sending, so every client but the one that asked went on drawing turns the
// session had discarded. It is now sent as well as persisted
// (`acp_session_impl/rewind.rs`), and these hold down the three decisions this
// client makes about it.
describe("a rewind somebody else performed", () => {
  test("the marker is read either way, and a malformed one is not a rewind", () => {
    expect(
      readRewindMarker({
        sessionUpdate: "rewind_marker",
        target_prompt_index: 2,
        created_at: "2026-09-09T10:05:00Z",
      }),
    ).toEqual({ targetPromptIndex: 2, createdAt: "2026-09-09T10:05:00Z" });
    // The fields are snake_case in the Rust to begin with, so this spelling is
    // the one on the wire; the other is read for the reason the points are.
    expect(readRewindMarker({ sessionUpdate: "rewind_marker", targetPromptIndex: 0 })).toEqual({
      targetPromptIndex: 0,
      createdAt: "",
    });
    expect(readRewindMarker({ sessionUpdate: "rewind_marker" })).toBeNull();
    expect(readRewindMarker({ sessionUpdate: "rewind_marker", target_prompt_index: -1 })).toBeNull();
    expect(readRewindMarker({ sessionUpdate: "agent_message_chunk" })).toBeNull();
    expect(readRewindMarker(null)).toBeNull();
  });

  test("it reloads without a cursor, and says so in the conversation", async () => {
    // Not the pager's answer to the same marker. The pager truncates its own
    // scrollback because it holds prompt indices on its blocks; this client
    // holds none, and the cursored reload that looks equivalent has the third
    // outcome the resume argument misses — a cursor can resolve against a line
    // that sits before the cut, and the empty tail then restores a screen the
    // agent has thrown away.
    const gateway = await attached();
    expect(texts(gateway)).toEqual([
      "first prompt",
      "first answer",
      "second prompt",
      "second answer",
    ]);
    expect(gateway.attached()!.resumption.cursor()).toBe("s1-3");

    socket.replay = AFTER;
    socket.marker(1);
    await settle();

    // Nothing was asked of the agent but the reload: this client did not rewind.
    expect(socket.calls("_x.ai/rewind/execute")).toHaveLength(0);
    const loads = socket.calls("session/load");
    expect(loads).toHaveLength(2);
    expect((loads[1]!["params"] as { _meta?: unknown })._meta).toBeUndefined();

    const entries = gateway.attached()!.transcript.entries;
    expect(entries.slice(0, 2).map((e) => (e as { text: string }).text)).toEqual([
      "first prompt",
      "first answer",
    ]);
    // Said in the conversation rather than on the status line, which the next
    // thing that happens would overwrite.
    const last = entries.at(-1)!;
    expect(last.kind).toBe("message");
    expect((last as { role: string }).role).toBe("notice");
    expect((last as { text: string }).text).toContain("Another client");
  });

  test("the marker for this client's own rewind is left to its own answer", async () => {
    // The marker goes out *before* the `x.ai/rewind/execute` response, so the
    // initiator sees its own first. Acting on it would reload ahead of the
    // answer that still has to hand the discarded prompt back to the composer,
    // and would tell the person a peer did what they just did themselves.
    const gateway = await attached();
    socket.replay = AFTER;
    socket.holdExecute = true;
    const view = mount(gateway);
    await settle();
    view.click(0);
    view.go();
    await settle();

    // The execute is in flight. This is the agent's own broadcast of it.
    socket.marker(1);
    await settle();
    expect(socket.calls("session/load")).toHaveLength(1);

    socket.release();
    await settle();
    // Exactly one reload, and it is the rewind's own.
    expect(socket.calls("session/load")).toHaveLength(2);
    expect(view.rewound()).toBe("second prompt");
    expect(texts(gateway)).toEqual(["first prompt", "first answer"]);
  });

  test("an open picker closes, because its rows name a timeline that is gone", async () => {
    // Every row and the confirm behind it are addressed by prompt index, and
    // after a peer's cut those indices name different turns or none. Re-reading
    // the list in place would silently swap what a half-finished gesture was
    // pointing at.
    const gateway = await attached();
    const view = mount(gateway);
    await settle();
    expect(view.rows()).toHaveLength(2);

    socket.replay = AFTER;
    socket.marker(1);
    await settle();
    expect(view.closed()).toBe(true);
  });

  test("a marker with no usable target reloads nothing", async () => {
    const gateway = await attached();
    socket.receive(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "_x.ai/session_notification",
        params: {
          sessionId: ENTRY.sessionId,
          update: { sessionUpdate: "rewind_marker", created_at: "2026-09-09T10:05:00Z" },
          _meta: { eventId: "s1-9" },
        },
      }),
    );
    await settle();
    expect(socket.calls("session/load")).toHaveLength(1);
    expect(texts(gateway)).toHaveLength(4);
  });

  test("the marker does not become the cursor", async () => {
    // A cursor is matched against the rewind-*filtered* log, and that filter
    // drops every marker (`session/storage/mod.rs:1602`) — so a cursor naming
    // one can never resolve, and recording it would guarantee the full replay
    // the cursor exists to avoid.
    const gateway = await attached();
    socket.replay = AFTER;
    socket.marker(1, 99);
    await settle();
    expect(gateway.attached()!.resumption.cursor()).not.toBe("s1-99");
  });
});
