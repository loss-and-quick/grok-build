import { For, Show, type JSX } from "solid-js";

import { blendToward, createTick, waitingBrightness } from "../animation.ts";
import type { PendingFolderTrust } from "../gateway.ts";
import { BULLET, DOT_FILLED, DOT_HOLLOW } from "../glyphs.ts";

/**
 * What each answer costs, in the pager's own words.
 *
 * Lifted from `xai-grok-pager/src/app/agent_view/interactions.rs` and
 * `acp_handler/interactions.rs`, not paraphrased. The decision a person makes
 * here has to mean the same thing it means in the terminal, and a second
 * wording is how two clients start to describe one grant differently.
 */
const CONSEQUENCE = {
  trust: "Project MCP servers, hooks, plugins and LSP are enabled for this workspace.",
  reject: "They stay off for this workspace, and you will not be asked again.",
  dismiss: "They stay off for this session. You'll be asked again next time you open it.",
} as const;

/**
 * The folder-trust card.
 *
 * The question is the pager's: a directory that is not in `trusted_folders.toml`
 * resolves untrusted, and its project's MCP servers, hooks, plugins, LSP and
 * permission rules are dropped *without saying so*. The terminal has drawn this
 * card since it declared `x.ai/folderTrust.interactive`; a browser that cannot
 * draw it is a browser whose sessions lose that configuration in silence.
 *
 * Three options, because there are three outcomes and they differ. Trust
 * persists a grant over the whole workspace. Reject leaves it gated and the
 * agent keeps its dedup key, so nothing asks again. Dismiss leaves it gated and
 * releases the key, so the next session in this workspace asks afresh — the
 * pager reaches that by dropping the response channel, and this reaches it by
 * answering with an error, which the agent reads the same way.
 *
 * The diamond pulses on the same "waiting on you" curve the permission card
 * uses: paused on you, not still working.
 */
export function FolderTrustCard(props: { pending: PendingFolderTrust }): JSX.Element {
  const tick = createTick();

  // The grant's scope is the workspace key, which can be an *ancestor* of the
  // session's root. When the two differ, agreeing trusts more than the
  // directory this session sits in, and that is not something to leave unsaid.
  const widerThanSession = (): boolean =>
    Boolean(props.pending.cwd) && props.pending.cwd !== props.pending.workspace;

  const options = () => [
    { id: "trust", label: "Trust this folder", consequence: CONSEQUENCE.trust },
    { id: "reject", label: "Leave untrusted", consequence: CONSEQUENCE.reject },
    { id: "dismiss", label: "Ask me next time", consequence: CONSEQUENCE.dismiss },
  ];

  const answer = (id: string): void => {
    if (id === "trust") props.pending.decide("trust");
    else if (id === "reject") props.pending.decide("reject");
    else props.pending.dismiss();
  };

  return (
    <div class="trust">
      <div class="trust-title">
        <span
          class="trust-diamond"
          aria-hidden="true"
          style={{
            color: blendToward(
              "var(--grok-bg-base)",
              "var(--grok-accent-user)",
              waitingBrightness(tick()),
            ),
          }}
        >
          {BULLET}
        </span>
        Trust the files in {props.pending.workspace}?
      </div>

      <Show when={widerThanSession()}>
        <div class="trust-scope">
          The grant covers that whole workspace; this session's root is{" "}
          <span class="trust-path">{props.pending.cwd}</span>.
        </div>
      </Show>

      {/* The agent says why the folder is gated at all: these are the
          repo-local config kinds it found. Rendered as sent, because the list
          is the agent's and a client that names its own would go stale. */}
      <Show when={props.pending.configKinds.length > 0}>
        <div class="trust-kinds">
          <span class="trust-kinds-label">Found here:</span>
          <For each={props.pending.configKinds}>
            {(kind) => <span class="trust-kind">{kind}</span>}
          </For>
        </div>
      </Show>

      <div class="trust-options">
        <For each={options()}>
          {(option, index) => (
            <button
              class={`trust-option trust-${option.id}`}
              type="button"
              onClick={() => answer(option.id)}
            >
              <span class="trust-key" aria-hidden="true">
                {index() + 1}
              </span>
              <span class="trust-marker" aria-hidden="true">
                ({index() === 0 ? DOT_FILLED : DOT_HOLLOW})
              </span>
              <span class="trust-label">{option.label}</span>
              <span class="trust-consequence">{option.consequence}</span>
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
