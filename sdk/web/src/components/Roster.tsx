import { A, useNavigate } from "@solidjs/router";
import { For, Show, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";
import { ACTIVITY_ROLE, directoryLabel, sessionLabel } from "../roster.ts";
import { cssVarName } from "../theme.ts";
import type { RosterEntry } from "../wire.ts";

/**
 * The roster, grouped by working directory.
 *
 * This grouping is the product's whole claim, so it is the first thing on the
 * screen. A session's root is a parameter of `session/new`, never process
 * state; the leader does not rewrite it, and roster entries carry it. "An
 * instance" is therefore a directory with sessions in it, and switching
 * instances is switching sessions — which is why each row is a link, not a
 * button: a session is a place you can bookmark and come back to.
 */
export function Roster(props: { gateway: Gateway; current: string | undefined }): JSX.Element {
  const groups = () => props.gateway.roster.groups();
  const navigate = useNavigate();

  // Creating a session and opening it are one gesture, but only the route
  // opens it: navigating is what attaches, so a new session arrives at a URL
  // that can be reloaded like any other.
  const createAndOpen = async (cwd: string): Promise<void> => {
    const sessionId = await props.gateway.createSession(cwd);
    if (sessionId) navigate(`/s/${sessionId}`);
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
                // Arbitrary directories need a picker, and a picker needs
                // `x.ai/fs/list` — which resolves against the leader's launch
                // directory, not the session's. So this client can only offer
                // roots the roster already names.
                onClick={() => void createAndOpen(group.cwd)}
              >
                + session here
              </button>
              <For each={group.sessions}>
                {(entry) => <Row entry={entry} current={props.current} />}
              </For>
            </section>
          )}
        </For>
      </Show>
    </div>
  );
}

function Row(props: { entry: RosterEntry; current: string | undefined }): JSX.Element {
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
      href={`/s/${props.entry.sessionId}`}
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
