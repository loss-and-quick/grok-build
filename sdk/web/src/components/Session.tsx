import { For, Match, Show, Switch, createSignal, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";

import { blendToward, createTick, waveBrightness } from "../animation.ts";
import { acceptRow, argumentHint, type CommandRow } from "../commands.ts";
import { ACCENT_BAR, BULLET, CHEVRON, PROMPT_ARROW, spinnerFrame } from "../glyphs.ts";
import {
  ellipsisFor,
  failureText,
  hasFailed,
  nextMode,
  toolTitle,
  truncate,
  type DisplayMode,
  type ToolCallFacts,
  type ToolTitle,
  type TruncatedOutput,
} from "../toolcall.ts";
import { typedResult, type TypedResult } from "../toolresult.ts";

import type { Gateway } from "../gateway.ts";
import { sessionLabel } from "../roster.ts";
import type { ToolCallEntry, TranscriptEntry } from "../transcript.ts";
import { CommandMenu, createCommandMenu } from "./CommandMenu.tsx";
import { Markdown } from "./Markdown.tsx";
import { Panel } from "./Panel.tsx";
import { PermissionCard } from "./PermissionCard.tsx";
import { Subagents } from "./Subagents.tsx";
import { ToolResult } from "./ToolResult.tsx";

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
            {/* The transcript is where the pager's third fallback reads from
                too: `entry_title` takes the first user prompt out of the
                scrollback before it gives up and names the session by its id. */}
            <h1 class="session-title">
              {sessionLabel(current().entry, firstPrompt(current().transcript.entries))}
            </h1>
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
              {(entry) => <Entry entry={entry} cwd={current().entry.cwd} tick={tick} />}
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

/** The first thing the user said, which is what an untitled session is called. */
function firstPrompt(entries: readonly TranscriptEntry[]): string | undefined {
  for (const entry of entries) {
    if (entry.kind === "message" && entry.role === "user") return entry.text;
  }
  return undefined;
}

function Entry(props: {
  entry: TranscriptEntry;
  cwd: string;
  tick: () => number;
}): JSX.Element {
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
            {/* An agent message and a thought both go through the pager's
                markdown pipeline — `AgentMessageBlock` and `ThinkingBlock` are
                two users of one `MarkdownContent`. A user turn does not: the
                pager renders what was typed as plain text with its own token
                spans, so marking it up here would show the author something
                other than what they wrote. */}
            <Show
              when={message().role !== "user"}
              fallback={<div class="message-text">{message().text}</div>}
            >
              <div class="message-text message-markdown">
                <Markdown text={message().text} />
              </div>
            </Show>
          </article>
        )}
      </Match>
      <Match when={props.entry.kind === "tool_call" ? props.entry : null}>
        {(call) => <ToolCall call={call()} cwd={props.cwd} tick={props.tick} />}
      </Match>
    </Switch>
  );
}

/**
 * One tool call: the title the terminal would have drawn, and a fold over its
 * output.
 *
 * **Output starts hidden**, which is the pager's own default for an agent tool
 * call (`default_display_mode` returns `Collapsed` in `read.rs`, `execute.rs`
 * and every sibling). That is the whole answer to a JSON blob pasted into the
 * transcript: the terminal never showed it either, and a client that prints
 * every byte of `content` is not being more informative than the terminal, it
 * is being less legible than it.
 */
function ToolCall(props: {
  call: ToolCallEntry;
  cwd: string;
  tick: () => number;
}): JSX.Element {
  const [mode, setMode] = createSignal<DisplayMode>("collapsed");
  const facts = (): ToolCallFacts => ({
    title: props.call.title,
    kind: props.call.toolKind,
    rawInput: props.call.rawInput,
    rawOutput: props.call.rawOutput,
    status: props.call.status,
    cwd: props.cwd,
  });
  const failed = (): boolean => hasFailed(facts());
  const body = (): string =>
    failed() ? failureText(facts(), props.call.output) : props.call.output;
  const lines = (): string[] => {
    const text = body();
    return text === "" ? [] : text.replace(/\n+$/, "").split("\n");
  };
  // A failed call has no typed result to draw: `ToolOutput::ReadFile` carries
  // `FileNotFound` instead of `FileContent`, and the terminal shows the error
  // text there too.
  const typed = (): TypedResult | null => (failed() ? null : typedResult(facts()));
  const openable = (): boolean => typed() !== null || lines().length > 0;
  const fold = (): TruncatedOutput => truncate(lines(), props.call.toolKind);
  const title = (): ToolTitle => toolTitle(facts());

  // A non-zero exit is a failure even when ACP calls the call completed, so
  // the class follows what the entry means rather than what the status field
  // literally said.
  return (
    <article class={`tool tool-${failed() ? "failed" : props.call.status}`}>
      {/* The accent rail waves while the call runs — the pager's own
          curve, speed and phase — and freezes flat when it finishes. */}
      <span
        class="tool-rail"
        aria-hidden="true"
        style={
          props.call.status === "in_progress"
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
      {/* The header is the control, because in the terminal the whole entry is:
          the pager folds an entry by acting on the row, not on a widget beside
          it. With no output there is nothing to fold, so it stops being a
          button rather than becoming a dead one. */}
      <Dynamic
        component={openable() ? "button" : "div"}
        class="tool-title"
        type={openable() ? "button" : undefined}
        aria-expanded={openable() ? mode() !== "collapsed" : undefined}
        onClick={
          openable() ? () => setMode(nextMode(props.call.toolKind, mode())) : undefined
        }
      >
        {/* Hovering a foldable row swaps the diamond for a chevron in place,
            which is how the terminal says an entry opens — the pager overwrites
            that one cell with `expandable_indicator_char` (`›`) on hover
            (`scrollback_pane.rs`), so the affordance costs no column. */}
        <span class="tool-bullet" aria-hidden="true">
          <span class="tool-bullet-mark">{BULLET}</span>
          <Show when={openable()}>
            <span class="tool-bullet-open">{CHEVRON}</span>
          </Show>
        </span>
        <Show when={title().verb}>
          {(verb) => <span class="tool-verb">{verb()}</span>}
        </Show>
        <For each={title().parts}>
          {(part) => <span class={`tool-part tool-part-${part.role}`}>{part.text}</span>}
        </For>
        <Show when={props.call.status === "in_progress"}>
          <span class="tool-spinner" aria-hidden="true">
            {spinnerFrame(props.tick())}
          </span>
        </Show>
      </Dynamic>
      {/* A description took the title line, so the command goes under it —
          `$` dim, exactly where the pager puts it, and only once the entry is
          open: `header_lines` is called with `include_command = false` while
          collapsed, for the same density this fold is for. */}
      <Show when={mode() !== "collapsed" && title().secondary}>
        {(command) => (
          <div class="tool-command">
            <span class="tool-command-mark" aria-hidden="true">
              $
            </span>
            {command()}
          </div>
        )}
      </Show>
      {/* The typed result when the wire carried one, and the text body
          otherwise. `rawOutput` is the whole result, not a
          summary of it, so drawing prose over it was this client throwing away
          what it had already been sent. */}
      <Show when={mode() !== "collapsed" && openable()}>
        <Show
          when={typed()}
          fallback={
            <pre class="tool-output" classList={{ "tool-error": failed() }}>
              <Show
                when={mode() === "truncated" && fold().hidden > 0}
                fallback={lines().join("\n")}
              >
                {fold().head.join("\n")}
                <span class="tool-ellipsis">
                  {"\n"}
                  {ellipsisFor(props.call.toolKind, fold().hidden)}
                  {"\n"}
                </span>
                {fold().tail.join("\n")}
              </Show>
            </pre>
          }
        >
          {(result) => <ToolResult result={result()} cwd={props.cwd} mode={mode()} />}
        </Show>
      </Show>
    </article>
  );
}
