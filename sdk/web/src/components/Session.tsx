import { For, Match, Show, Switch, createSignal, type JSX } from "solid-js";

import { blendToward, createTick, waveBrightness } from "../animation.ts";
import { acceptRow, argumentHint, type CommandRow } from "../commands.ts";
import { ACCENT_BAR, BULLET, PROMPT_ARROW, spinnerFrame } from "../glyphs.ts";

import type { Gateway } from "../gateway.ts";
import { sessionLabel } from "../roster.ts";
import type { TranscriptEntry } from "../transcript.ts";
import { CommandMenu, createCommandMenu } from "./CommandMenu.tsx";
import { Panel } from "./Panel.tsx";
import { PermissionCard } from "./PermissionCard.tsx";
import { Subagents } from "./Subagents.tsx";

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
  const tick = createTick();
  const menu = createCommandMenu(() => props.gateway.commands());
  // The composer's text as state, not only as a DOM value: the menu reads it on
  // every edit, and an accepted row writes it back.
  const [line, setLine] = createSignal("");

  const reread = (): void => {
    if (!composer) return;
    setLine(composer.value);
    menu.sync(composer.value, composer.selectionStart);
  };

  const send = (): void => {
    const text = composer?.value.trim() ?? "";
    if (!text) return;
    if (composer) composer.value = "";
    setLine("");
    menu.sync("", 0);
    // A slash command is sent as ordinary prompt text, because that *is* the
    // dispatch path: the shell resolves the leading token against the same
    // catalog it advertised, and a plugin's command reaches that plugin's own
    // code over `command_invoke`. Nothing here needs a second method.
    void props.gateway.prompt(text);
  };

  /** Take a row into the composer and put the caret after it. */
  const take = (chosen?: CommandRow): void => {
    const row = chosen ?? menu.accept();
    if (!row || !composer) return;
    const next = acceptRow(composer.value, row);
    composer.value = next.text;
    composer.setSelectionRange(next.caret, next.caret);
    reread();
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

          {/* Between the panels and the transcript: a fan-out is state the
              transcript would scroll away, and the composer must stay put. */}
          <Subagents gateway={props.gateway} />

          <div class="transcript">
            <For each={current().transcript.entries}>
              {(entry) => <Entry entry={entry} tick={tick} />}
            </For>
          </div>

          <form
            class="composer"
            classList={{ running: props.gateway.status() === "running…" }}
            onSubmit={(event) => {
              event.preventDefault();
              send();
            }}
          >
            {/* Above the input rather than below it, the way the pager stacks
                its dropdown over the prompt: the list grows upward, so a long
                catalog never pushes the line being typed off the screen. */}
            <CommandMenu menu={menu} onTake={(row) => take(row)} />
            <textarea
              class="prompt-input"
              rows={3}
              placeholder={
                argumentHint(props.gateway.commands(), line()) ?? "Message this session…"
              }
              ref={composer}
              onInput={reread}
              onClick={reread}
              onKeyUp={reread}
              onFocus={() => menu.revive()}
              onBlur={() => menu.dismiss()}
              onKeyDown={(event) => {
                if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault();
                  send();
                  return;
                }
                if (!menu.open()) return;
                // While the menu is up these keys belong to it. Enter takes the
                // highlighted row rather than sending, which is the pager's own
                // rule: Enter on `/doctor` accepts and opens the argument phase,
                // and only a second Enter runs it.
                if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                  event.preventDefault();
                  menu.move(event.key === "ArrowDown" ? 1 : -1);
                  return;
                }
                if (event.key === "Tab" || event.key === "Enter") {
                  event.preventDefault();
                  take();
                  return;
                }
                if (event.key === "Escape") {
                  event.preventDefault();
                  menu.dismiss();
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

function Entry(props: { entry: TranscriptEntry; tick: () => number }): JSX.Element {
  return (
    <Switch>
      <Match when={props.entry.kind === "message" ? props.entry : null}>
        {(message) => (
          <article class={`message message-${message().role}`}>
            {/* The pager marks a user turn with the same prompt arrow the
                composer shows, and leaves the agent's own messages unprefixed —
                the rail carries the role instead. */}
            <Show when={message().role === "user"}>
              <span class="message-arrow" aria-hidden="true">
                {PROMPT_ARROW}
              </span>
            </Show>
            <div class="message-text">{message().text}</div>
          </article>
        )}
      </Match>
      <Match when={props.entry.kind === "tool_call" ? props.entry : null}>
        {(call) => (
          <article class={`tool tool-${call().status}`}>
            {/* The accent rail waves while the call runs — the pager's own
                curve, speed and phase — and freezes flat when it finishes. */}
            <span
              class="tool-rail"
              aria-hidden="true"
              style={
                call().status === "in_progress"
                  ? {
                      color: blendToward(
                        "var(--grok-bg-base)",
                        "var(--grok-accent-running)",
                        waveBrightness(props.tick()),
                      ),
                    }
                  : undefined
              }
            >
              {ACCENT_BAR}
            </span>
            <div class="tool-title">
              <span class="tool-bullet" aria-hidden="true">
                {BULLET}
              </span>
              {call().title}
              <Show when={call().status === "in_progress"}>
                <span class="tool-spinner" aria-hidden="true">
                  {spinnerFrame(props.tick())}
                </span>
              </Show>
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
