import { For, Show, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";

/**
 * The settings the wire says a non-terminal client should draw.
 *
 * The filter reads each row's `surface` field; it carries no list of keys to
 * skip. That is the point of the field: a row that stops having a wire form
 * stops being drawn by the TUI in the same build, rather than silently going
 * missing from one client. Read-only — writing is `x.ai/settings/set`.
 */
export function Settings(props: { gateway: Gateway }): JSX.Element {
  const state = props.gateway.settings;
  return (
    <details class="settings">
      <summary>
        Settings — {state.rows.length} shown, {state.terminalOnly} terminal-only
      </summary>
      <For each={state.rows}>
        {(row) => (
          <div class="setting-row">
            <span class="setting-label" title={row.key}>
              {row.label}
            </span>
            <Show
              when={state.locks[row.key]}
              fallback={<span class="setting-value">{String(state.values[row.key] ?? "—")}</span>}
            >
              {(lock) => (
                // The read-only lock is a `stat` on the leader's machine, which
                // a second client cannot perform — so it rides the response
                // instead of being recomputed, and is shown rather than guessed.
                <span class="setting-value setting-locked">locked — {lock().reason}</span>
              )}
            </Show>
          </div>
        )}
      </For>
    </details>
  );
}
