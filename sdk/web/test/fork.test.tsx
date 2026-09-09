// Fork, pinned against `fork_session_params` and the shape of the call it makes.
//
// Three things here are easy to get wrong in a way nothing would notice.
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
//   3. **`targetPromptIndex` cuts on the other side of the turn it names than
//      the field of that name on `x.ai/rewind/execute` does.** A fork at N
//      keeps prompt N — `truncate_for_prompt_by` cuts when `user_turn_count >
//      target_prompt_index + 1` (`session/storage/mod.rs:1023-1046`) — and a
//      rewind to N discards it. Reading either one as the other gives a child
//      that is one turn short or one turn long, which nothing else would catch.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { cleanup, render } from "@solidjs/testing-library";

import type { SocketLike } from "../src/client.ts";
import { WHOLE_CONVERSATION } from "../src/components/ForkPicker.tsx";
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
      // The same enumeration of turns the rewind picker reads. It is not about
      // rewinding: the agent generates one row per prompt `0..current_prompt_index`
      // (`acp_session_impl/rewind.rs:44`), and it is the only list of a
      // session's turns on the wire.
      "_x.ai/rewind/points": {
        rewind_points: [
          { prompt_index: 0, prompt_preview: "first prompt", has_file_changes: true },
          { prompt_index: 1, prompt_preview: "second prompt" },
        ],
      },
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

/** The session with its fork dialog open, and handles on what it drew. */
async function openPicker(gateway: Gateway) {
  const opened: string[] = [];
  const { container } = render(() =>
    Session({ gateway, rail: false, onForked: (id) => opened.push(id) }),
  );
  (container.querySelector(".session-fork") as HTMLButtonElement).click();
  await settle();
  return {
    container,
    opened,
    rows: (): string[] =>
      [...container.querySelectorAll(".fork-row-preview")].map((n) => n.textContent ?? ""),
    click: (at: number): void =>
      (container.querySelectorAll<HTMLButtonElement>(".fork-row")[at] as HTMLButtonElement).click(),
    note: (): string => container.querySelector(".fork-note")?.textContent ?? "",
    failure: (): string => container.querySelector(".fork-failed-message")?.textContent ?? "",
  };
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

  test("`targetPromptIndex` is absent for the whole conversation, not null", () => {
    // Absent and null are read identically by the agent (`Option<usize>` behind
    // `#[serde(default)]`), and only one of them is what the terminal sends.
    // The distinction is kept because the two are different statements: no cut,
    // against a cut of nothing.
    expect(Object.keys(forkParams(ENTRY))).not.toContain("targetPromptIndex");
    expect(Object.keys(forkParams(ENTRY, undefined))).not.toContain("targetPromptIndex");
  });

  test("a branch point rides along, and zero is one of them", () => {
    // The field exists on `ForkSessionRequest`, `copy_session_data` implements
    // it, and until now `fork_session_params` never set it — the pager still
    // answers `/fork --at` with "not supported in this version". Zero is a real
    // target, not an absence: it is the fork that keeps the first turn only.
    expect(forkParams(ENTRY, 1)).toEqual({
      sourceSessionId: "parent",
      sourceCwd: "/home/me/repo",
      newCwd: "/home/me/repo",
      sessionKind: "fork",
      targetPromptIndex: 1,
    });
    expect(forkParams(ENTRY, 0).targetPromptIndex).toBe(0);
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
    const view = await openPicker(gateway);
    view.click(0);
    await settle();
    expect(view.opened).toEqual([]);
    expect(view.failure()).toContain("named no new session");
    // The dialog stays open on the failure rather than closing over it, so the
    // reason is still there to read; `Dismiss` puts the list back.
    expect(view.container.querySelector(".fork-picker")).not.toBeNull();
  });

  test("the whole conversation is the first row, and sends no index", async () => {
    // The act this button used to perform, still one keystroke away.
    const gateway = await attached();
    const view = await openPicker(gateway);
    expect(view.rows()[0]).toBe(WHOLE_CONVERSATION);
    view.click(0);
    await settle();
    expect(view.opened).toEqual(["child"]);
    expect(socket.calls("_x.ai/session/fork")[0]!["params"]).toEqual({
      sourceSessionId: "parent",
      sourceCwd: "/home/me/repo",
      newCwd: "/home/me/repo",
      sessionKind: "fork",
    });
  });

  test("a turn below it forks from that turn, inclusive of it", async () => {
    // The list is the rewind picker's, newest first, and the row means the
    // opposite of what it means there: `truncate_for_prompt_by` cuts when
    // `user_turn_count > target_prompt_index + 1` (`session/storage/mod.rs:1023`),
    // so the child keeps prompts `0..=N`. Measured as well as read — a
    // six-update source forked at index 1 produced a child with four.
    const gateway = await attached();
    const view = await openPicker(gateway);
    expect(view.rows()).toEqual([WHOLE_CONVERSATION, "second prompt", "first prompt"]);
    view.click(2);
    await settle();
    expect(view.opened).toEqual(["child"]);
    expect(socket.calls("_x.ai/session/fork")[0]!["params"]).toMatchObject({
      targetPromptIndex: 0,
    });
  });

  test("there is no confirm step, because a fork discards nothing", async () => {
    // The rewind picker asks twice: a click is one whole gesture and the act
    // there destroys a conversation. This one creates a session beside the one
    // on screen and leaves it alone, so the second question would be asking
    // permission for nothing.
    const gateway = await attached();
    const view = await openPicker(gateway);
    view.click(1);
    expect(socket.calls("_x.ai/session/fork")).toHaveLength(1);
  });

  test("the button is not disabled mid-turn, which is the terminal's decision", async () => {
    // `dispatch_fork` refuses only a session that has no id yet
    // (`app/dispatch/session/fork.rs:48-51`).
    const gateway = await attached();
    const { container } = render(() => Session({ gateway, rail: false }));
    const button = container.querySelector<HTMLButtonElement>(".session-fork")!;
    expect(button.disabled).toBe(false);
    expect(button.textContent).toContain("Fork");
  });
});

describe("the list is not this dialog's own", () => {
  test("the note says which way the cut falls, once, rather than per row", async () => {
    // The one thing a list of turns cannot say about itself, and it is the
    // opposite of what the same row means in the rewind picker.
    const gateway = await attached();
    const view = await openPicker(gateway);
    expect(view.note()).toContain("up to and including");
    expect(view.note()).toContain("left exactly as it is");
  });

  test("a peer's rewind closes it, because its rows name a timeline that is gone", async () => {
    // Same answer the rewind picker gives to the same marker. Forking from a
    // turn that has just been discarded would branch from a different turn than
    // the one under the cursor, which is worse than closing.
    const gateway = await attached();
    const view = await openPicker(gateway);
    expect(view.rows().length).toBeGreaterThan(0);
    socket.receive(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "_x.ai/session_notification",
        params: {
          sessionId: ENTRY.sessionId,
          update: {
            sessionUpdate: "rewind_marker",
            target_prompt_index: 0,
            created_at: "2026-09-09T10:05:00Z",
          },
          _meta: { eventId: "parent-9" },
        },
      }),
    );
    await settle();
    expect(view.rows()).toEqual([]);
  });
});
