import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";

import { Decisions } from "../src/components/Decisions.tsx";
import { createDecisions, isHere, slotOrder, type Decision } from "../src/decisions.ts";
import type { Gateway, PendingFolderTrust, PendingPermission } from "../src/gateway.ts";

function permission(toolCallId: string, sessionId: string): PendingPermission {
  return {
    toolCallId,
    title: `Write ${toolCallId}`,
    request: {
      sessionId,
      toolCall: { toolCallId },
      options: [
        { optionId: "yes", name: "Yes", kind: "allow_once" },
        { optionId: "no", name: "No", kind: "reject_once" },
      ],
    },
    answer: () => {},
  };
}

function trust(sessionId: string): PendingFolderTrust {
  return {
    sessionId,
    cwd: "/repo",
    workspace: "/repo",
    configKinds: [],
    decide: () => {},
    dismiss: () => {},
  };
}

const NO_CHILDREN: ReadonlySet<string> = new Set();

describe("the single modal slot follows the pager's own precedence", () => {
  test("a permission outranks a folder-trust question, whichever arrived first", () => {
    const asTrust: Decision = { kind: "trust", key: "t", sessionId: "s", pending: trust("s") };
    const asPermission: Decision = {
      kind: "permission",
      key: "p",
      sessionId: "s",
      pending: permission("p", "s"),
    };
    expect(slotOrder([asTrust, asPermission]).map((d) => d.key)).toEqual(["p", "t"]);
  });

  test("two of a kind keep the order they arrived in", () => {
    const first: Decision = {
      kind: "permission",
      key: "a",
      sessionId: "s",
      pending: permission("a", "s"),
    };
    const second: Decision = {
      kind: "permission",
      key: "b",
      sessionId: "s",
      pending: permission("b", "s"),
    };
    expect(slotOrder([first, second]).map((d) => d.key)).toEqual(["a", "b"]);
  });
});

describe("what counts as being about the session on screen", () => {
  const asPermission = (sessionId: string): Decision => ({
    kind: "permission",
    key: `p:${sessionId}`,
    sessionId,
    pending: permission("p", sessionId),
  });

  test("a permission for another session is not, so it waits rather than interrupts", () => {
    expect(isHere(asPermission("other"), "attached", NO_CHILDREN)).toBe(false);
  });

  test("a permission from a subagent is, because it stops the parent's turn", () => {
    expect(isHere(asPermission("child"), "attached", new Set(["child"]))).toBe(true);
  });

  test("a folder-trust question always is: the leader sends it once and never replays it", () => {
    const asTrust: Decision = {
      kind: "trust",
      key: "t",
      sessionId: "somewhere",
      pending: trust("somewhere"),
    };
    expect(isHere(asTrust, null, NO_CHILDREN)).toBe(true);
  });
});

describe("parking sets a question aside without answering it", () => {
  test("the parked one leaves the slot and the next one takes it", () => {
    const [permissions] = createSignal<PendingPermission[]>([
      permission("a", "s"),
      permission("b", "s"),
    ]);
    const decisions = createDecisions({
      permissions,
      folderTrusts: () => [],
      attachedSessionId: () => "s",
      childSessions: () => NO_CHILDREN,
    });

    expect(decisions.modal()?.key).toBe("permission:a");
    decisions.park("permission:a");
    expect(decisions.modal()?.key).toBe("permission:b");
    // Parked is not answered: it is still one of the questions this turn has
    // stopped on, which is what keeps the composer disabled.
    expect(decisions.here()).toHaveLength(2);
    decisions.unpark("permission:a");
    expect(decisions.modal()?.key).toBe("permission:a");
  });

  test("parking every question empties the slot but not the list", () => {
    const decisions = createDecisions({
      permissions: () => [permission("a", "s")],
      folderTrusts: () => [],
      attachedSessionId: () => "s",
      childSessions: () => NO_CHILDREN,
    });
    decisions.park("permission:a");
    expect(decisions.modal()).toBeNull();
    expect(decisions.here()).toHaveLength(1);
  });
});

/** Only the fields `Decisions` reads; the rest of a gateway is not involved. */
function fakeGateway(
  permissions: PendingPermission[],
  folderTrusts: PendingFolderTrust[],
  sessionId: string | null,
): Gateway {
  return {
    permissions,
    folderTrusts,
    status: () => "connected",
    attached: () =>
      sessionId
        ? {
            entry: { sessionId, title: null },
            subagents: { childSessions: () => NO_CHILDREN },
          }
        : null,
  } as unknown as Gateway;
}

/**
 * Mount `Decisions` over a page, and take the page away again afterwards.
 *
 * The cleanup is in a `finally` on purpose: a failed assertion inside must not
 * leave a mounted modal behind, because a modal that outlives its test holds a
 * document-level key listener and an `inert` attribute over everything the next
 * suite renders.
 */
function overPage(gateway: Gateway, body: (layout: HTMLElement) => void): void {
  const layout = document.createElement("div");
  layout.className = "layout";
  document.body.append(layout);
  const { unmount } = render(() => <Decisions gateway={gateway} />);
  try {
    body(layout);
  } finally {
    unmount();
    layout.remove();
  }
}

describe("the modal keeps the promise it makes", () => {
  test("it claims aria-modal and makes the page behind it inert, not merely covered", () => {
    // Counted rather than checked on the element this test appended: a suite
    // that ran earlier may have left a `.layout` of its own in the document,
    // and the page has exactly one, so what matters is that exactly one is
    // inert while the question is up and none is afterwards.
    const inertLayouts = (): number => document.querySelectorAll(".layout[inert]").length;
    expect(inertLayouts()).toBe(0);
    overPage(fakeGateway([permission("a", "s")], [], "s"), () => {
      const dialog = document.querySelector<HTMLElement>(".decision-modal");
      expect(dialog?.getAttribute("aria-modal")).toBe("true");
      expect(dialog?.getAttribute("role")).toBe("dialog");
      expect(inertLayouts()).toBe(1);
    });
    // The claim is withdrawn with the card. A layout left inert after the
    // question is answered is a page nobody can use.
    expect(inertLayouts()).toBe(0);
  });

  test("Escape parks rather than dismisses, so the question stays answerable", () => {
    overPage(fakeGateway([permission("a", "s")], [], "s"), () => {
      expect(document.querySelector(".decision-modal")).not.toBeNull();
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      expect(document.querySelector(".decision-modal")).toBeNull();
      // Still on the page, still unanswered, still reachable.
      expect(document.querySelector(".decision-resume")).not.toBeNull();
    });
  });

  test("a permission for a session that is not on screen never steals the slot", () => {
    overPage(fakeGateway([permission("a", "elsewhere")], [], "on-screen"), () => {
      expect(document.querySelector(".decision-modal")).toBeNull();
      const pointer = document.querySelector<HTMLAnchorElement>(".decision-elsewhere");
      expect(pointer?.getAttribute("href")).toBe("/s/elsewhere");
    });
  });

  test("a folder-trust question is modal even with no session attached", () => {
    overPage(fakeGateway([], [trust("s")], null), () => {
      expect(document.querySelector(".decision-modal")).not.toBeNull();
      expect(document.querySelector(".trust-options")).not.toBeNull();
    });
  });
});
