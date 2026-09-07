import { For, Match, Show, Switch, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";
import { sessionLabel } from "../roster.ts";
import type { TranscriptEntry } from "../transcript.ts";
import { Panel } from "./Panel.tsx";
import { PermissionCard } from "./PermissionCard.tsx";

/**
 * One attached session: its panels, its transcript, and the composer.
 *
 * `<For>` over `entries` is keyed by reference, and a streaming reply grows the
 * *last* entry's `text` in place rather than appending a new one, so an
 * `agent_message_chunk` updates a single text node. That is the whole reason
 * this client is fine-grained rather than virtual-DOM.
 */
export function Session(props: { gateway: Gateway }): JSX.Element {
  let composer: HTMLTextAreaElement | undefined;

  const send = (): void => {
    const text = composer?.value.trim() ?? "";
    if (!text) return;
    if (composer) composer.value = "";
    void props.gateway.prompt(text);
  };

  return (
    <Show
      when={props.gateway.attached()}
      fallback={<p class="empty">Pick a session on the left.</p>}
    >
      {(current) => (
        <>
          <header class="session-header">
            <h1 class="session-title">{sessionLabel(current().entry)}</h1>
            <div class="session-cwd">{current().entry.cwd}</div>
          </header>

          <div class="permissions">
            <For each={props.gateway.permissions}>
              {(pending) => <PermissionCard pending={pending} />}
            </For>
          </div>

          <div class="panels">
            <For each={Object.values(current().transcript.panels)}>
              {(panel) => (
                <Panel
                  plugin={panel.plugin}
                  viewModel={panel.viewModel}
                  onAction={(action) => void props.gateway.panelAction(panel.plugin, action)}
                />
              )}
            </For>
          </div>

          <div class="transcript">
            <For each={current().transcript.entries}>{(entry) => <Entry entry={entry} />}</For>
          </div>

          <form
            class="composer"
            onSubmit={(event) => {
              event.preventDefault();
              send();
            }}
          >
            <textarea
              class="prompt-input"
              rows={3}
              placeholder="Message this session…"
              ref={composer}
              onKeyDown={(event) => {
                if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault();
                  send();
                }
              }}
            />
            <button class="send" type="submit">
              Send
            </button>
          </form>
        </>
      )}
    </Show>
  );
}

function Entry(props: { entry: TranscriptEntry }): JSX.Element {
  return (
    <Switch>
      <Match when={props.entry.kind === "message" ? props.entry : null}>
        {(message) => (
          <article class={`message message-${message().role}`}>
            <div class="message-role">{message().role}</div>
            <div class="message-text">{message().text}</div>
          </article>
        )}
      </Match>
      <Match when={props.entry.kind === "tool_call" ? props.entry : null}>
        {(call) => (
          <article class={`tool tool-${call().status}`}>
            <div class="tool-title">
              {call().title} — {call().status}
            </div>
            <Show when={call().output}>
              <pre class="tool-output">{call().output}</pre>
            </Show>
          </article>
        )}
      </Match>
    </Switch>
  );
}
