import { A, useNavigate } from "@solidjs/router";
import { For, Show, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";
import { sessionHref } from "../instances.ts";
import { ACTIVITY_ROLE, directoryLabel, sessionLabel } from "../roster.ts";
import { cssVarName } from "../theme.ts";
import type { RosterEntry } from "../wire.ts";

/**
 * The roster, grouped by working directory.
 *
 * This grouping is the product's whole claim, so it is the first thing on the
 * screen. A session's root is a parameter of `session/new`, never process
 * state; the leader does not rewrite it, and roster entries carry it — which is
 * why each row is a link and not a button: a session is a place you can
 * bookmark and come back to.
 *
 * The whole of this list belongs to one socket. `x.ai/sessions/list` is one
 * leader's answer and `roster.replace` wipes what came before it, so the
 * machine it came from is named *above* this column rather than being a level
 * inside it — see `instances.ts`.
 */
export function Roster(props: {
  gateway: Gateway;
  current: string | undefined;
  /**
   * The instance these sessions belong to, written into every link.
   *
   * A session id is unique on a leader and not between leaders, so a link
   * without it is only half an address. Old links stay valid — no `i` means
   * "whichever instance this tab is on" — and new ones say which machine they
   * were written on, which is what lets a page opened on the wrong one explain
   * itself instead of showing an empty screen.
   */
  instance: string | null;
}): JSX.Element {
  const groups = () => props.gateway.roster.groups();
  const navigate = useNavigate();

  // Creating a session and opening it are one gesture, but only the route
  // opens it: navigating is what attaches, so a new session arrives at a URL
  // that can be reloaded like any other.
  const createAndOpen = async (cwd: string): Promise<void> => {
    const sessionId = await props.gateway.createSession(cwd);
    if (sessionId) navigate(sessionHref(sessionId, props.instance));
  };

  return (
    <div class="roster-list">
      <Show
        when={groups().length > 0}
        fallback={<p class="empty">No sessions on this leader yet.</p>}
      >
        <For each={groups()}>
          {(group) => (
            <section class="roster-group">
              <A class="roster-cwd" href={`/d/${encodeURIComponent(group.cwd)}`} title={group.cwd}>
                {directoryLabel(group.cwd)}
              </A>
              <div class="roster-cwd-full">{group.cwd}</div>
              <button
                class="roster-new"
                type="button"
                title={`New session in ${group.cwd}`}
                // The shortcut, not the limit. `x.ai/fs/list` takes an absolute
                // path and walks it as-is — confinement is off in local mode —
                // and `initialize` answers with `currentWorkingDirectory`, so a
                // picker rooted anywhere the user can read is a client-side
                // change with no wire behind it. This button is just the case
                // where the root is already on screen.
                onClick={() => void createAndOpen(group.cwd)}
              >
                + session here
              </button>
              <For each={group.sessions}>
                {(entry) => (
                  <Row entry={entry} current={props.current} instance={props.instance} />
                )}
              </For>
            </section>
          )}
        </For>
      </Show>
    </div>
  );
}

function Row(props: {
  entry: RosterEntry;
  current: string | undefined;
  instance: string | null;
}): JSX.Element {
  const meta = () =>
    [
      props.entry.isWorktree ? "worktree" : "",
      props.entry.resident ? "" : "dormant",
      props.entry.modelId ?? "",
    ]
      .filter(Boolean)
      .join(" · ");

  return (
    <A
      class="roster-row"
      classList={{ current: props.current === props.entry.sessionId }}
      href={sessionHref(props.entry.sessionId, props.instance)}
    >
      <span
        class="roster-dot"
        title={props.entry.activity}
        style={{ background: `var(${cssVarName(ACTIVITY_ROLE[props.entry.activity])})` }}
      />
      <span class="roster-label">{sessionLabel(props.entry)}</span>
      <span class="roster-meta">{meta()}</span>
    </A>
  );
}
