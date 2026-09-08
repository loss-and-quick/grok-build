// Which unanswered decision owns the screen, and which one waits its turn.
//
// Two things on the wire block the agent on a human: `session/request_permission`
// and `x.ai/folder_trust/request`. Until now this client drew both as ordinary
// cards in the flow, which meant a streaming transcript could push a question
// the turn is parked on out of view, and nothing said so.
//
// The terminal does not have that problem, and the reason is worth copying
// rather than re-deciding. The pager draws a permission **into the prompt
// slot** — `let perm_area = layout.prompt` (`agent_view/render.rs:2674-2687`) —
// so it is not something you can scroll past; it is the thing standing where
// you would type. It arbitrates with a single-slot enum,
// `BlockingCard::{Permission, CancelTurn, Question, McpElicitation}`
// (`agent_view/key_owner.rs:11-16`), where **Permission outranks Question**
// (`:104-116`) and folder trust is a Question
// (`acp_handler/interactions.rs:504-514`). And it can be **parked**: Tab or
// Space hands the keyboard back to the scrollback and the card stays up,
// unanswered (`key_owner.rs:19-35`).
//
// This module is that arbiter, ported. It decides nothing about presentation
// beyond who holds the one slot; the components draw what it names.
import { createSignal } from "solid-js";

import type { Gateway, PendingFolderTrust, PendingPermission } from "./gateway.ts";

/** The two blocking questions, in the pager's own precedence order. */
export type DecisionKind = "permission" | "trust";

/**
 * `BlockingCard`'s ordering, as a number.
 *
 * Lower wins, and the gap between them is the pager's: a permission is a turn
 * that has stopped mid-flight, a folder-trust question is a session that has
 * not started properly. The first is more urgent because something is already
 * half-done and waiting.
 */
export function rank(kind: DecisionKind): number {
  return kind === "permission" ? 0 : 1;
}

/** One unanswered question, whichever kind it is. */
export type Decision =
  | { kind: "permission"; key: string; sessionId: string; pending: PendingPermission }
  | { kind: "trust"; key: string; sessionId: string; pending: PendingFolderTrust };

/**
 * Is this decision about what is on screen?
 *
 * **Only permissions are asked this**, and the asymmetry is the wire's, not a
 * preference. A permission is a broadcast interaction: the leader sends it to
 * every subscriber, caches it, and replays it to a client that attaches later
 * (`is_interaction_request`, `leader/server.rs:492-516`), so a permission for a
 * session you are not looking at will still be there when you go there.
 * Deferring it costs nothing.
 *
 * Folder trust is deliberately none of those — the same file says so and says
 * why: it "is not a tool call and emits neither, so listing it would grant the
 * broadcast alone, with no caching, no replay-on-attach"
 * (`leader/server.rs:497-505`). It is routed to one client, once. If this
 * client defers it, nobody ever sees it again and a workspace's MCP servers,
 * hooks, plugins and LSP stay off in silence for the half hour the agent waits.
 *
 * So a folder-trust question is always here. A permission is here when it
 * belongs to the attached session or to one of the subagents it spawned —
 * those arrive under the *child's* session id but block the parent's turn.
 */
export function isHere(
  decision: Decision,
  attachedSessionId: string | null,
  childSessions: ReadonlySet<string>,
): boolean {
  if (decision.kind === "trust") return true;
  if (!attachedSessionId) return false;
  return decision.sessionId === attachedSessionId || childSessions.has(decision.sessionId);
}

/**
 * Sort into the order the single slot is handed out in.
 *
 * Kind first, arrival second. Arrival is the array's own order: both stores are
 * appended to, so index *is* age, and no timestamp has to be invented for a
 * comparison the list already encodes.
 */
export function slotOrder(decisions: readonly Decision[]): Decision[] {
  return decisions
    .map((decision, index) => ({ decision, index }))
    .sort((a, b) =>
      rank(a.decision.kind) - rank(b.decision.kind) || a.index - b.index,
    )
    .map((entry) => entry.decision);
}

export interface Decisions {
  /** Everything unanswered, in slot order. */
  all(): Decision[];
  /** The one that owns the modal, or `null` when nothing does. */
  modal(): Decision | null;
  /** Unanswered and about the session on screen, whether parked or not. */
  here(): Decision[];
  /** Unanswered and about some other session, keyed by that session's id. */
  elsewhere(): Map<string, Decision[]>;
  /** Set the question aside without answering it: the pager's Tab/Space. */
  park(key: string): void;
  /** Bring a parked question back to the modal. */
  unpark(key: string): void;
  parked(key: string): boolean;
}

/**
 * The arbiter over the two pending stores.
 *
 * Parking is keyed by the same string the decision is keyed by, and a key that
 * is no longer pending is dropped on the next read — an answered question
 * cannot leave a stale park behind, and one that is re-asked comes back
 * unparked, which is the right default for a question being asked a second
 * time.
 */
export function createDecisions(source: {
  permissions: () => readonly PendingPermission[];
  folderTrusts: () => readonly PendingFolderTrust[];
  attachedSessionId: () => string | null;
  childSessions: () => ReadonlySet<string>;
}): Decisions {
  const [parkedKeys, setParkedKeys] = createSignal<ReadonlySet<string>>(new Set());

  const all = (): Decision[] =>
    slotOrder([
      ...source.permissions().map(
        (pending): Decision => ({
          kind: "permission",
          key: `permission:${pending.toolCallId}`,
          sessionId: pending.request.sessionId ?? "",
          pending,
        }),
      ),
      ...source.folderTrusts().map(
        (pending): Decision => ({
          kind: "trust",
          key: `trust:${pending.sessionId}`,
          sessionId: pending.sessionId,
          pending,
        }),
      ),
    ]);

  const here = (): Decision[] =>
    all().filter((decision) =>
      isHere(decision, source.attachedSessionId(), source.childSessions()),
    );

  const elsewhere = (): Map<string, Decision[]> => {
    const attached = source.attachedSessionId();
    const children = source.childSessions();
    const bySession = new Map<string, Decision[]>();
    for (const decision of all()) {
      if (isHere(decision, attached, children)) continue;
      const bucket = bySession.get(decision.sessionId) ?? [];
      bucket.push(decision);
      bySession.set(decision.sessionId, bucket);
    }
    return bySession;
  };

  const parked = (key: string): boolean => parkedKeys().has(key);

  return {
    all,
    here,
    elsewhere,
    parked,
    modal: () => here().find((decision) => !parked(decision.key)) ?? null,
    park: (key) => setParkedKeys((keys) => new Set(keys).add(key)),
    unpark: (key) =>
      setParkedKeys((keys) => {
        const next = new Set(keys);
        next.delete(key);
        return next;
      }),
  };
}

/**
 * The one arbiter a gateway has.
 *
 * Two surfaces read it — the modal above everything (`Decisions.tsx`) and the
 * composer that must not let you type past a question (`Session.tsx`) — and
 * they have to agree about what is parked, or Escape would hide a card from one
 * of them and not the other. Keyed by the gateway rather than kept in a module
 * variable so a second gateway is a second arbiter and a test can have its own.
 */
const arbiters = new WeakMap<Gateway, Decisions>();

export function decisionsFor(gateway: Gateway): Decisions {
  const existing = arbiters.get(gateway);
  if (existing) return existing;
  const made = createDecisions({
    permissions: () => gateway.permissions,
    folderTrusts: () => gateway.folderTrusts,
    attachedSessionId: () => gateway.attached()?.entry.sessionId ?? null,
    childSessions: () => gateway.attached()?.subagents.childSessions() ?? new Set<string>(),
  });
  arbiters.set(gateway, made);
  return made;
}
