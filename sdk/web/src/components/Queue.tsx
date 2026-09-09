import { For, Show, createSignal, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";
import type { QueueRow } from "../queue.ts";
import type { QueueImageWire } from "../wire.ts";

/**
 * The manifest as one `title`, in the terminal's own numbering.
 *
 * `displayNumber` is the `N` of the row text's `[Image #N]` placeholder and
 * deliberately not the list position — the numbers need not be dense — so this
 * prints what the text says rather than what the array counts.
 */
function imageList(images: readonly QueueImageWire[]): string {
  return images
    .map((image) => `Image #${image.displayNumber ?? "?"}${image.mimeType ? ` (${image.mimeType})` : ""}`)
    .join("\n");
}

/**
 * The prompts this session is holding, and what may be done to them.
 *
 * The pager's queue pane, in a browser: `#N`, the first non-empty line, a
 * `(+N lines)` remark when the row is hiding some, and the kind carried in the
 * marker rather than in a word (`views/queue_pane.rs`, `build_styled`).
 *
 * ## Nothing here is optimistic
 *
 * Every control below sends an ext-**notification** and then waits. There is no
 * reply to any of them — the confirming `x.ai/queue/changed` broadcast is the
 * whole feedback channel, and the handlers rebroadcast even when they changed
 * nothing — so a row that may not be moved simply does not move. The terminal
 * removes a row locally before the agent answers and reconciles afterwards;
 * this client does not, because the one thing it could get wrong is showing a
 * withdrawal that never happened.
 *
 * ## What is offered is what the session will honour
 *
 * `row.mutable` is the session's own bit, read off the wire
 * (`QueueEntryWire::editable`), not inferred from the kind. That distinction is
 * the whole reason the field was added: a client guessing from `kind` offers
 * controls the session silently no-ops, and the guess was right only because
 * exactly one origin is protected and its kind is named after it.
 */
export function Queue(props: { gateway: Gateway }): JSX.Element {
  const rows = (): readonly QueueRow[] => props.gateway.attached()?.queue.rows ?? [];
  /** The row being edited, and the draft standing in for its text. */
  const [editing, setEditing] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal("");

  /**
   * Open the edit box, and tell the session to hold the row while it is open.
   *
   * The hold is not decoration. While a turn runs the session promotes the
   * queued prefix into it as interjections and stops at a row under an edit
   * hold (`prompt_queue.rs`, `promote_queued_as_interjections`); without one, a
   * row being edited here can be promoted mid-edit and sent with the text it
   * had before.
   */
  const open = (row: QueueRow): void => {
    const held = editing();
    if (held !== null && held !== row.id) props.gateway.queueReleaseEdit(held);
    setEditing(row.id);
    setDraft(row.text);
    props.gateway.queueHoldEdit(row.id);
  };

  const close = (): void => {
    const held = editing();
    if (held !== null) props.gateway.queueReleaseEdit(held);
    setEditing(null);
    setDraft("");
  };

  /**
   * Save the draft.
   *
   * `x.ai/queue/edit` carries no version and is last-write-wins: the session
   * bumps the version and records this client as `last_editor`
   * (`prompt_queue.rs`, `apply_queued_prompt_edit`). What it does *not* touch
   * is the row's images — the blocks are rebuilt as the new text plus every
   * `ContentBlock::Image` the row already had — which is why the box below says
   * so rather than leaving a person to wonder.
   */
  const save = (row: QueueRow): void => {
    const text = draft().trim();
    if (text !== "" && text !== row.text) props.gateway.queueEdit(row.id, text);
    close();
  };

  /** How the running turn is named in the send-now explanation, if there is one. */
  const running = (): string | null => props.gateway.attached()?.queue.running?.line ?? null;

  return (
    <section class="queue" aria-label="Queued prompts">
      <ol class="queue-rows">
        <For each={rows()}>
          {(row, at) => (
            <li class="queue-row" classList={{ "queue-protected": !row.mutable }}>
              <span class="queue-number" aria-hidden="true">
                #{row.number}
              </span>
              <Show
                when={editing() === row.id}
                fallback={
                  <>
                    {/* The whole prompt as a `title`, for the reader whose
                        column cut it short — the same answer the rail's rows
                        give, and for the same reason: an ellipsis that cannot
                        be recovered in place is a row that has to be opened
                        somewhere else to be read. */}
                    <span class={`queue-text queue-${row.kind}`} title={row.text}>
                      {/* The terminal's own `! ` for a bash row. Its cron rows
                          carry `↻`, which is a codepoint typed into
                          `queue_pane.rs` and not in the generated glyph table,
                          so this client cannot draw it and says the kind in
                          colour instead. */}
                      <Show when={row.kind === "bash"}>
                        <span class="queue-mark" aria-hidden="true">
                          !
                        </span>
                      </Show>
                      <span class="queue-line">{row.line}</span>
                      <Show when={row.hidden > 0}>
                        <span class="queue-hidden">
                          (+{row.hidden} {row.hidden === 1 ? "line" : "lines"})
                        </span>
                      </Show>
                    </span>
                    {/* The images the session is holding for this row. A
                        listing, not a transfer: the bytes never ride the
                        broadcast. Absent when the agent never said, which is
                        not the same as a row with none — so nothing is drawn
                        rather than "0 images". */}
                    <Show when={row.images?.length ? row.images : null}>
                      {(images) => (
                        <span class="queue-images" title={imageList(images())}>
                          {images().length} {images().length === 1 ? "image" : "images"}
                        </span>
                      )}
                    </Show>
                    <Show when={row.mutable} fallback={<span class="queue-pinned">held by the agent that queued it</span>}>
                      <span class="queue-actions">
                        <button
                          type="button"
                          class="queue-move"
                          disabled={at() === 0}
                          title="Move this prompt one place earlier"
                          onClick={() => props.gateway.queueMove(row.id, "up")}
                        >
                          Up
                        </button>
                        <button
                          type="button"
                          class="queue-move"
                          disabled={at() === rows().length - 1}
                          title="Move this prompt one place later"
                          onClick={() => props.gateway.queueMove(row.id, "down")}
                        >
                          Down
                        </button>
                        <button
                          type="button"
                          class="queue-now"
                          title={
                            running()
                              ? `Send this into the turn already running (${running()}) instead of waiting for it`
                              : "Run this next, ahead of everything else queued"
                          }
                          onClick={() => props.gateway.queueSendNow(row.id, row.version)}
                        >
                          Send now
                        </button>
                        <button
                          type="button"
                          class="queue-edit"
                          title="Rewrite this prompt before it runs"
                          onClick={() => open(row)}
                        >
                          Edit
                        </button>
                        <button
                          type="button"
                          class="queue-withdraw"
                          title="Take this prompt out of the queue. It never runs."
                          onClick={() => props.gateway.queueRemove(row.id, row.version)}
                        >
                          Withdraw
                        </button>
                      </span>
                    </Show>
                  </>
                }
              >
                <div class="queue-editor">
                  <textarea
                    class="queue-draft"
                    aria-label={`Prompt #${row.number}`}
                    rows={Math.min(row.hidden + 1, 8)}
                    value={draft()}
                    onInput={(event) => setDraft(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") {
                        event.preventDefault();
                        close();
                        return;
                      }
                      // Enter sends, Shift+Enter breaks the line: the composer's
                      // own rule, so one text box in this page does not behave
                      // differently from the other.
                      if (event.key === "Enter" && !event.shiftKey) {
                        event.preventDefault();
                        save(row);
                      }
                    }}
                  />
                  <Show when={row.images && row.images.length > 0}>
                    <p class="queue-note">
                      The images stay with this prompt; only the text is replaced.
                    </p>
                  </Show>
                  <div class="queue-editor-actions">
                    <button type="button" class="queue-save" onClick={() => save(row)}>
                      Save
                    </button>
                    <button type="button" class="queue-cancel" onClick={() => close()}>
                      Cancel
                    </button>
                  </div>
                </div>
              </Show>
            </li>
          )}
        </For>
      </ol>
      {/* Offered only where there is more than one to take back, because with a
          single row it is the same act as the Withdraw beside it under a name
          that sounds larger. */}
      <Show when={rows().filter((row) => row.mutable).length > 1}>
        <div class="queue-foot">
          <button
            type="button"
            class="queue-clear"
            title="Take every prompt out of the queue. The running turn is not touched."
            onClick={() => props.gateway.queueClear()}
          >
            Withdraw all
          </button>
        </div>
      </Show>
    </section>
  );
}
