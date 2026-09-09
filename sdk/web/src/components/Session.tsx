import { For, Match, Show, Switch, createSignal, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";

import { blendToward, createTick, waveBrightness } from "../animation.ts";
import { acceptRow, argumentHint, type CommandRow } from "../commands.ts";
import {
  ACCENT_BAR,
  BALLOT_X,
  BULLET,
  CHEVRON,
  GLYPH_WARNING,
  PROMPT_ARROW,
  spinnerFrame,
} from "../glyphs.ts";
import {
  defaultMode,
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

import {
  FRAME_BUDGET,
  blockFor,
  frameSize,
  human,
  isRefusal,
  overBudget,
  take as takeAttachment,
  type Attachment,
  type Refusal,
} from "../attach.ts";
import { decisionsFor } from "../decisions.ts";
import type { Gateway } from "../gateway.ts";
import { sessionLabel } from "../roster.ts";
import type { ToolCallEntry, TranscriptEntry } from "../transcript.ts";
import { detectAt, isDirMode, type FuzzyMatch } from "../filesearch.ts";
import { CommandMenu, createCommandMenu } from "./CommandMenu.tsx";
import { FileMenu, createFileMenu } from "./FileMenu.tsx";
import { Markdown } from "./Markdown.tsx";
import { ModelPicker } from "./ModelPicker.tsx";
import { Panel } from "./Panel.tsx";
import { Rail } from "./Rail.tsx";

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
export function Session(props: {
  gateway: Gateway;
  rail: boolean;
  /**
   * What to draw with nothing attached.
   *
   * Passed in because the reason there is nothing on screen is the route's to
   * know, not this component's: "pick one from the left" and "that session is
   * on another machine" are the same absence here and completely different
   * things to be told.
   */
  fallback?: JSX.Element;
}): JSX.Element {
  let composer: HTMLTextAreaElement | undefined;
  const tick = createTick();
  // The same arbiter the modal reads, so "parked" means one thing on both
  // surfaces: what Escape sets aside up there is what this offers to reopen.
  const decisions = decisionsFor(props.gateway);
  /** Questions this session's turn has stopped on, parked or not. */
  const blocking = () => decisions.here();
  // Files waiting to go with the next prompt, and the ones that were handed over
  // and turned down. Refusals are state rather than a toast because the reason a
  // file was not taken has to survive long enough to be read and acted on.
  const [attachments, setAttachments] = createSignal<Attachment[]>([]);
  const [refusals, setRefusals] = createSignal<Refusal[]>([]);
  const [dragging, setDragging] = createSignal(false);
  let picker: HTMLInputElement | undefined;
  const menu = createCommandMenu(() => props.gateway.commands());
  const files = createFileMenu(props.gateway.fileSearch);
  // The composer's text as state, not only as a DOM value: the menu reads it on
  // every edit, and an accepted row writes it back.
  const [line, setLine] = createSignal("");
  // Whether the `@`-token under the caret asks for directories only. The pager
  // draws the trailing `/` from the query's mode rather than each row's kind, so
  // the flag has to reach the list.
  const [dirMode, setDirMode] = createSignal(false);

  const reread = (): void => {
    if (!composer) return;
    setLine(composer.value);
    menu.sync(composer.value, composer.selectionStart);
    files.sync(composer.value, composer.selectionStart);
    const at = detectAt(composer.value, composer.selectionStart);
    setDirMode(at !== null && isDirMode(at));
  };

  /** Put an accepted path into the composer and follow it with the caret. */
  const takeFile = (result: ReturnType<typeof files.accept>): void => {
    if (!result || !composer) return;
    composer.value = result.text;
    composer.setSelectionRange(result.caret, result.caret);
    reread();
  };

  const send = (): void => {
    const text = composer?.value.trim() ?? "";
    const carried = attachments();
    if (!text && carried.length === 0) return;
    // Checked here rather than trusted per file: the whole prompt is one
    // WebSocket frame, and the gateway does not *refuse* an oversized one, it
    // closes the socket. Sending it would look like the connection dying for no
    // reason. See `attach.ts` for the measurement.
    if (overBudget(text, carried)) {
      setRefusals([
        {
          name: `${carried.length} attachment${carried.length === 1 ? "" : "s"}`,
          reason: `Together they make a ${human(frameSize(text, carried))} message, over the ${human(FRAME_BUDGET)} one message may be. Send them across two turns.`,
        },
      ]);
      return;
    }
    if (composer) composer.value = "";
    setLine("");
    setAttachments([]);
    setRefusals([]);
    menu.sync("", 0);
    files.sync("", 0);
    setDirMode(false);
    // A slash command is sent as ordinary prompt text, because that *is* the
    // dispatch path: the shell resolves the leading token against the same
    // catalog it advertised, and a plugin's command reaches that plugin's own
    // code over `command_invoke`. Nothing here needs a second method.
    void props.gateway.prompt(text, carried.map(blockFor));
  };

  /**
   * Take files from wherever they came: the picker, a drop, or a paste.
   *
   * All three land here because a person does not think of them as three
   * things. What each file *becomes* is decided in `attach.ts`; this only reads
   * the bytes and keeps the answers.
   */
  const absorb = async (incoming: readonly File[]): Promise<void> => {
    if (incoming.length === 0) return;
    const kept: Attachment[] = [];
    const turned: Refusal[] = [];
    for (const file of incoming) {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const result = takeAttachment(file.name || "pasted", file.type, bytes, crypto.randomUUID());
      if (isRefusal(result)) turned.push(result);
      else kept.push(result);
    }
    setAttachments([...attachments(), ...kept]);
    setRefusals(turned);
  };

  const drop = (event: DragEvent): void => {
    const dropped = [...(event.dataTransfer?.files ?? [])];
    if (dropped.length === 0) return;
    event.preventDefault();
    setDragging(false);
    void absorb(dropped);
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
      fallback={props.fallback ?? <p class="empty">Pick a session on the left.</p>}
    >
      {(current) => (
        // Three areas, one grid, and the composer is a child of it rather than
        // of the column above it. That is what lets the rail be a column beside
        // the session when there is room and a dock directly above the prompt
        // when there is not — the pager's own geometry, which is where the rail
        // came from — without moving anything in the DOM as the window changes
        // size. A move would rebuild whatever a plugin panel was holding, which
        // is the thing `Panel.tsx` exists to prevent.
        <div class="session-body" classList={{ railed: props.rail }}>
          <div class="session-flow">
          <header class="session-header">
            {/* The transcript is where the pager's third fallback reads from
                too: `entry_title` takes the first user prompt out of the
                scrollback before it gives up and names the session by its id. */}
            <h1 class="session-title">
              {sessionLabel(current().entry, firstPrompt(current().transcript.entries))}
            </h1>
            <div class="session-cwd">{current().entry.cwd}</div>
            {/* In the header rather than by the composer: the terminal keeps the
                model in its status bar, where it is a property of the session
                rather than of the message being typed. */}
            <ModelPicker gateway={props.gateway} />
          </header>

          {/* Where a panel goes when the rail is off: a stack over the
              transcript, which is where every panel went before the rail
              existed. The agent decides — `dock_enabled` rides
              `x.ai/settings/update` to every client — so this is not a second
              design kept alive beside the first, it is the off position of the
              agent's own switch. */}
          <Show when={!props.rail}>
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
          </Show>

          {/* Between the panels and the transcript: a fan-out is state the
              transcript would scroll away, and the composer must stay put.
              Only when the rail is off, and for the same reason the panels
              above it are: with the rail on, the fan-out is a section of the
              dock like the terminal's own, and drawing it in both places would
              put two live copies of the same state on one screen. */}
          <Show when={!props.rail}>
            <Subagents gateway={props.gateway} />
          </Show>

          <div class="transcript">
            <For each={current().transcript.entries}>
              {(entry) => <Entry entry={entry} cwd={current().entry.cwd} tick={tick} />}
            </For>
          </div>
          </div>

          <Show when={props.rail}>
            <Rail gateway={props.gateway} />
          </Show>

          <form
            class="composer"
            classList={{
              running: props.gateway.status() === "running…",
              blocked: blocking().length > 0,
              dragging: dragging(),
            }}
            onSubmit={(event) => {
              event.preventDefault();
              send();
            }}
            // A drop needs both handlers: without `dragover` being prevented the
            // browser navigates to the file instead, which loses the page.
            onDragOver={(event) => {
              if (!event.dataTransfer?.types.includes("Files")) return;
              event.preventDefault();
              setDragging(true);
            }}
            onDragLeave={(event) => {
              if (event.currentTarget.contains(event.relatedTarget as Node)) return;
              setDragging(false);
            }}
            onDrop={drop}
          >
            {/* The pager does not let you type past a question it is blocked
                on: the permission card is drawn *into the prompt slot*
                (`agent_view/render.rs:2674-2687`), so the composer is not
                there to type into. A browser cannot take the box away without
                taking a half-written draft with it, so it says what is true
                instead — the turn has stopped, and here is the way back to the
                question. */}
            <Show when={blocking()[0]}>
              {(blocked) => (
                <div class="composer-blocked">
                  <span class="composer-blocked-mark" aria-hidden="true">
                    {GLYPH_WARNING}
                  </span>
                  <span class="composer-blocked-text">
                    {blocking().length === 1
                      ? "This turn has stopped on a question."
                      : `This turn has stopped on ${blocking().length} questions.`}
                  </span>
                  <button
                    class="composer-blocked-open"
                    type="button"
                    onClick={() => decisions.unpark(blocked().key)}
                  >
                    Answer it
                  </button>
                </div>
              )}
            </Show>
            {/* Above the input rather than below it, the way the pager stacks
                its dropdown over the prompt: the list grows upward, so a long
                catalog never pushes the line being typed off the screen. */}
            {/* Only one of the two is ever up: the caret is inside a `/` token
                or an `@` token, never both, and where it is inside an `@` the
                file list is the more specific answer. */}
            <Show
              when={files.open()}
              fallback={<CommandMenu menu={menu} onTake={(row) => take(row)} />}
            >
              <FileMenu
                menu={files}
                dirMode={dirMode()}
                root={props.gateway.fileSearch.root()}
                onTake={(row: FuzzyMatch) =>
                  takeFile(files.accept(composer?.value ?? "", row))
                }
              />
            </Show>
            {/* Above the input, in the row the blocked banner uses: what is
                going with this message belongs beside the message, not under
                the button that sends it. */}
            <Show when={attachments().length > 0 || refusals().length > 0}>
              <div class="attachments">
                <For each={attachments()}>
                  {(file) => (
                    <span class="attachment" classList={{ [`attachment-${file.kind}`]: true }}>
                      <span class="attachment-name">{file.name}</span>
                      <span class="attachment-size">{human(file.size)}</span>
                      <button
                        class="attachment-drop"
                        type="button"
                        aria-label={`Remove ${file.name}`}
                        onClick={() =>
                          setAttachments(attachments().filter((other) => other.id !== file.id))
                        }
                      >
                        {BALLOT_X}
                      </button>
                    </span>
                  )}
                </For>
                {/* Named and explained. A file that quietly did not attach is
                    the failure this whole path exists to avoid — the agent
                    drops an undersized image mid-turn and only says so in a
                    notice nobody is looking at. */}
                <For each={refusals()}>
                  {(refused) => (
                    <span class="attachment attachment-refused">
                      <span class="attachment-name">{refused.name}</span>
                      <span class="attachment-reason">{refused.reason}</span>
                    </span>
                  )}
                </For>
              </div>
            </Show>
            <textarea
              class="prompt-input"
              rows={3}
              placeholder={
                argumentHint(props.gateway.commands(), line()) ?? "Message this session…"
              }
              ref={composer}
              onInput={reread}
              // A screenshot in the clipboard is the commonest attachment there
              // is, and the terminal already takes it that way — `grok wrap`
              // ships the host pasteboard image over a private OSC so a remote
              // pager can paste one (`wrap_clipboard_image.rs`).
              onPaste={(event) => {
                const pasted = [...(event.clipboardData?.files ?? [])];
                if (pasted.length === 0) return;
                event.preventDefault();
                void absorb(pasted);
              }}
              onClick={reread}
              onKeyUp={reread}
              onFocus={() => {
                menu.revive();
                files.revive();
              }}
              onBlur={() => {
                menu.dismiss();
                files.dismiss();
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault();
                  send();
                  return;
                }
                // The `@`-list first, and its keys are the pager's own
                // `handle_file_search_key`: the arrows and their Ctrl aliases,
                // a half-page on PageUp/PageDown, Tab or Enter to take the row,
                // and the right arrow to step into a directory without
                // committing it. Escape closes the list and leaves the typed
                // text exactly where it is, which is what the terminal does.
                if (files.open()) {
                  const control = event.ctrlKey;
                  const down =
                    event.key === "ArrowDown" ||
                    (control && (event.key === "n" || event.key === "j"));
                  const up =
                    event.key === "ArrowUp" ||
                    (control && (event.key === "p" || event.key === "k"));
                  if (down || up) {
                    event.preventDefault();
                    files.move(down ? 1 : -1);
                    return;
                  }
                  if (event.key === "PageDown" || (control && event.key === "d")) {
                    event.preventDefault();
                    files.page(1);
                    return;
                  }
                  if (event.key === "PageUp" || (control && event.key === "u")) {
                    event.preventDefault();
                    files.page(-1);
                    return;
                  }
                  if (event.key === "Tab" || event.key === "Enter") {
                    event.preventDefault();
                    takeFile(files.accept(composer?.value ?? ""));
                    return;
                  }
                  if (event.key === "ArrowRight") {
                    event.preventDefault();
                    takeFile(files.drill(composer?.value ?? ""));
                    return;
                  }
                  if (event.key === "Escape") {
                    event.preventDefault();
                    files.dismiss();
                    return;
                  }
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
            <input
              class="attach-picker"
              type="file"
              multiple
              ref={picker}
              onChange={(event) => {
                void absorb([...(event.currentTarget.files ?? [])]);
                // Cleared so choosing the same file twice running still fires.
                event.currentTarget.value = "";
              }}
            />
            <button
              class="attach"
              type="button"
              aria-label="Attach files"
              onClick={() => picker?.click()}
            >
              Attach
            </button>
            <button class="send" type="submit" disabled={blocking().length > 0}>
              Send
            </button>
          </form>
        </div>
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
  // A user's fold, or nothing: the opening mode is derived, so an edit whose
  // result has not arrived yet is not frozen collapsed by a signal initialised
  // before the frame that says it succeeded.
  const [folded, setFolded] = createSignal<DisplayMode | null>(null);
  const facts = (): ToolCallFacts => ({
    title: props.call.title,
    kind: props.call.toolKind,
    rawInput: props.call.rawInput,
    rawOutput: props.call.rawOutput,
    status: props.call.status,
    cwd: props.cwd,
  });
  const failed = (): boolean => hasFailed(facts());
  const mode = (): DisplayMode => folded() ?? defaultMode(props.call.toolKind, failed());
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
          openable() ? () => setFolded(nextMode(props.call.toolKind, mode())) : undefined
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
      {/* The typed result when the wire carried one — a diff, hits, a gutter —
          and the text body otherwise. `rawOutput` is the whole result, not a
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
