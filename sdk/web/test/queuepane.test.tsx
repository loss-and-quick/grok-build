// The queue on screen, and the rail section it lives in.
//
// What is pinned here is the thing the wire half was built for: the controls a
// row offers are the ones the session will honour, read off `editable` and not
// guessed from the kind. The rest is the shape of the section — a header with a
// count and a body, appearing and disappearing with the count, which is
// `dock.rs`'s Queued exactly.
import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";

import { Queue } from "../src/components/Queue.tsx";
import { Rail } from "../src/components/Rail.tsx";
import type { Gateway } from "../src/gateway.ts";
import { createQueue } from "../src/queue.ts";
import { createSubagents } from "../src/subagents.ts";
import { createTasks } from "../src/tasks.ts";
import { createTranscript } from "../src/transcript.ts";
import type { QueueChanged, RosterEntry } from "../src/wire.ts";

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

const QUEUE: QueueChanged = {
  sessionId: ENTRY.sessionId,
  entries: [
    { id: "a", version: 3, kind: "prompt", text: "rename the parser\nand its tests" },
    { id: "b", version: 0, kind: "parent_agent_message", text: "from the parent", editable: false },
    {
      id: "c",
      version: 1,
      kind: "bash",
      text: "look at [Image #2]",
      images: [{ displayNumber: 2, mimeType: "image/png" }],
    },
  ],
  runningPromptId: "r0",
  runningText: "rewrite the parser",
};

interface Sent {
  verb: string;
  id?: string;
  version?: number;
  text?: string;
  direction?: string;
}

function mount(changed: QueueChanged = QUEUE) {
  const queue = createQueue();
  queue.apply(changed);
  const sent: Sent[] = [];
  const gateway = {
    attached: () => ({
      entry: ENTRY,
      transcript: createTranscript(),
      subagents: createSubagents(ENTRY.sessionId),
      tasks: createTasks(),
      queue,
    }),
    sessionInfo: () => null,
    queueEdit: (id: string, text: string) => sent.push({ verb: "edit", id, text }),
    queueRemove: (id: string, version: number) => sent.push({ verb: "remove", id, version }),
    queueClear: () => sent.push({ verb: "clear" }),
    queueMove: (id: string, direction: "up" | "down") =>
      sent.push({ verb: "reorder", id, direction }),
    queueSendNow: (id: string, version: number) => sent.push({ verb: "interject", id, version }),
    queueHoldEdit: (id: string) => sent.push({ verb: "hold", id }),
    queueReleaseEdit: (id: string) => sent.push({ verb: "release", id }),
  } as unknown as Gateway;
  return { queue, sent, gateway };
}

function pane(changed: QueueChanged = QUEUE) {
  const held = mount(changed);
  const { container } = render(() => Queue({ gateway: held.gateway }));
  return { ...held, container };
}

/** The buttons on one row, by their label. */
function actions(container: HTMLElement, at: number): string[] {
  const row = container.querySelectorAll(".queue-row")[at]!;
  return [...row.querySelectorAll("button")].map((button) => button.textContent ?? "");
}

function click(container: HTMLElement, at: number, label: string): void {
  const row = container.querySelectorAll(".queue-row")[at]!;
  const button = [...row.querySelectorAll("button")].find((b) => b.textContent === label);
  if (!button) throw new Error(`no ${label} on row ${at}`);
  button.click();
}

describe("the queue on screen", () => {
  test("each row is numbered, shows its first line, and says how much it is hiding", () => {
    const { container } = pane();
    expect([...container.querySelectorAll(".queue-number")].map((n) => n.textContent)).toEqual([
      "#1",
      "#2",
      "#3",
    ]);
    expect(container.querySelector(".queue-line")?.textContent).toBe("rename the parser");
    expect(container.querySelector(".queue-hidden")?.textContent).toBe("(+1 line)");
  });

  test("the whole prompt is recoverable in place from the row that clipped it", () => {
    // The rail's rows carry the same `title` for the same reason: an ellipsis
    // that has to be opened elsewhere to be read is a row that cannot be read.
    const { container } = pane();
    expect(container.querySelector(".queue-text")?.getAttribute("title")).toBe(
      "rename the parser\nand its tests",
    );
  });

  test("a row's images are counted and named, never drawn", () => {
    // A manifest, not a transfer: the bytes stay in the session's copy of the
    // prompt blocks and never ride the broadcast this row came from.
    const { container } = pane();
    const images = container.querySelectorAll(".queue-images")[0]!;
    expect(images.textContent).toBe("1 image");
    expect(images.getAttribute("title")).toBe("Image #2 (image/png)");
  });

  test("a row the agent said nothing about is not described as having no images", () => {
    // `undefined` is an agent too old to answer, and it is not `[]`. A client
    // that read absence as emptiness would draw image prompts as bare text and
    // never find out it was wrong.
    const { container } = pane({
      sessionId: ENTRY.sessionId,
      entries: [{ id: "a", version: 0, kind: "prompt", text: "look at [Image #1]" }],
    });
    expect(container.querySelector(".queue-images")).toBeNull();
  });
});

describe("what a row lets you do to it", () => {
  test("a mutable row offers all four; a protected one offers none and says why", () => {
    // `editable` is the same bit the mutation handlers gate on. Five disabled
    // buttons would read as this client being broken rather than as the row
    // being held by the agent that queued it.
    const { container } = pane();
    expect(actions(container, 0)).toEqual(["Up", "Down", "Send now", "Edit", "Withdraw"]);
    expect(actions(container, 1)).toEqual([]);
    expect(container.querySelectorAll(".queue-pinned")).toHaveLength(1);
  });

  test("the controls send the row's id and the version it was last seen at", () => {
    const { container, sent } = pane();
    click(container, 0, "Withdraw");
    click(container, 0, "Send now");
    click(container, 2, "Up");
    expect(sent).toEqual([
      { verb: "remove", id: "a", version: 3 },
      { verb: "interject", id: "a", version: 3 },
      { verb: "reorder", id: "c", direction: "up" },
    ]);
  });

  test("an edit holds the row while it is open and releases it either way", () => {
    // Without the hold, a row being edited here can be promoted into the
    // running turn mid-edit and sent with the text it had before
    // (`promote_queued_as_interjections` stops at a row under an edit hold).
    const { container, sent } = pane();
    click(container, 0, "Edit");
    expect(sent).toEqual([{ verb: "hold", id: "a" }]);
    const draft = container.querySelector<HTMLTextAreaElement>(".queue-draft")!;
    draft.value = "rename it properly";
    draft.dispatchEvent(new Event("input", { bubbles: true }));
    click(container, 0, "Save");
    expect(sent).toEqual([
      { verb: "hold", id: "a" },
      { verb: "edit", id: "a", text: "rename it properly" },
      { verb: "release", id: "a" },
    ]);
  });

  test("cancelling an edit sends no edit, and still releases the hold", () => {
    const { container, sent } = pane();
    click(container, 0, "Edit");
    click(container, 0, "Cancel");
    expect(sent).toEqual([
      { verb: "hold", id: "a" },
      { verb: "release", id: "a" },
    ]);
  });

  test("an unchanged draft is not an edit", () => {
    // The handler would bump the version and record a new `last_editor` for a
    // text nobody changed, which is a row that looks touched to every other
    // client watching it.
    const { container, sent } = pane();
    click(container, 0, "Edit");
    click(container, 0, "Save");
    expect(sent.filter((frame) => frame.verb === "edit")).toEqual([]);
  });

  test("the edit box says the images are not going anywhere", () => {
    // `apply_queued_prompt_edit` rebuilds the blocks as the new text plus every
    // `ContentBlock::Image` the row already had, so an edit box that looked
    // like it was throwing them away would be lying.
    const { container } = pane();
    click(container, 2, "Edit");
    expect(container.querySelector(".queue-note")?.textContent).toContain("images stay");
  });

  test("withdraw-all is offered only where it is not the button beside it", () => {
    const { container } = pane();
    expect(container.querySelector(".queue-clear")).not.toBeNull();
    const one = pane({
      sessionId: ENTRY.sessionId,
      entries: [{ id: "a", version: 0, kind: "prompt", text: "one" }],
    });
    expect(one.container.querySelector(".queue-clear")).toBeNull();
  });
});

describe("Queued, as a section of the rail", () => {
  const railWith = (changed: QueueChanged) => {
    const held = mount(changed);
    const { container } = render(() => Rail({ gateway: held.gateway }));
    return { ...held, container };
  };

  test("it is a header with a count and the queue as its body", () => {
    // `dock.rs`: `DockData::rows(Queued)` is `&[]` and `visual_rows` pushes its
    // header alone — "the Queued section embeds the queue pane as its body".
    // So there are no rail rows under it, and there is no row cap either.
    const { container } = railWith(QUEUE);
    expect([...container.querySelectorAll(".rail-label")].map((n) => n.textContent)).toEqual([
      "Queued",
    ]);
    expect(container.querySelector(".rail-count")?.textContent).toBe("3");
    expect(container.querySelectorAll('[data-rail-item^="r:queued:"]')).toHaveLength(0);
    expect(container.querySelectorAll(".queue-row")).toHaveLength(3);
  });

  test("an empty queue draws no section, and an otherwise empty rail draws nothing", () => {
    // The dock's emptiness rule, which reaches Queued because Queued carries a
    // count — the rule is about counts and never was about lists.
    const { container } = railWith({ sessionId: ENTRY.sessionId, entries: [] });
    expect(container.querySelector(".rail")).toBeNull();
  });

  test("there is no Open beside it", () => {
    // Unlike the three sections above it, nothing is being held back for want
    // of room: a queue you can see two of is one you cannot reorder.
    const { container } = railWith(QUEUE);
    expect(container.querySelector(".rail-open")).toBeNull();
  });
});
