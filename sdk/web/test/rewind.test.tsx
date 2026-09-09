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
  readRewindPoints,
  readRewindResult,
  rewindConfirmTitle,
  rewindExecuteParams,
  rewindLabel,
} from "../src/rewind.ts";
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

  private listeners = new Map<string, ((event: never) => void)[]>();

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
    // And it says the thing neither client can see from the rows.
    expect(view.warning()).toContain("until it reloads");
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
      attached: () => ({ entry: ENTRY, transcript, subagents: createSubagents(ENTRY.sessionId) }),
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
