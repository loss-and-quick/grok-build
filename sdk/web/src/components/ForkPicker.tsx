import { For, Match, Switch, createEffect, createSignal, on, onMount, type JSX } from "solid-js";

import { focusInto, trapFocus } from "../focus.ts";
import type { Gateway } from "../gateway.ts";
import { PROMPT_ARROW } from "../glyphs.ts";
import { rewindLabel, type RewindPoint } from "../rewind.ts";

/**
 * Where to branch the conversation from.
 *
 * `targetPromptIndex` has been on `ForkSessionRequest` and implemented in
 * `copy_session_data` the whole time, and no client has ever sent it:
 * `fork_session_params` never sets it and the pager answers `/fork --at` with
 * "not supported in this version". So this is a capability the agent has and
 * neither client offers, and drawing it is a decision rather than a
 * transcription.
 *
 * ## Why this is not the rewind picker with different words
 *
 * The list is the same list — `x.ai/rewind/points` is not about rewinding, it
 * is the only enumeration of a session's turns on the wire, one row per prompt
 * with the agent's own preview — so the *reader* is shared and this file has
 * none of its own. The dialog is not, and the reason is that the two ask
 * opposite questions of the row under the cursor.
 *
 *   - **Rewind at N discards prompt N.** The agent restores the state from
 *     before it ran and hands its text back for the composer
 *     (`acp_session_impl/rewind.rs`, `prompt_texts.get(target_index)`).
 *   - **Fork at N keeps prompt N.** `truncate_for_prompt_by` cuts at the first
 *     line of the *next* turn — it stops when `user_turn_count >
 *     target_prompt_index + 1` (`session/storage/mod.rs:1023-1046`) — so the
 *     child holds prompts `0..=N`. Measured as well as read: a six-update
 *     source forked at index 1 produced a child with four.
 *
 * One component with a flag would have had that inversion threaded through
 * every string it draws, and the strings are the entire product of the
 * decision. Two of the three other differences follow from the same place: this
 * has **no confirm step**, because the confirm in the rewind picker is there to
 * supply a second half to a gesture that destroys something and a fork destroys
 * nothing; and its rows carry **no `edited files` remark**, because that
 * remark is about a rewind leaving files where a turn put them, and a fork does
 * not touch files at all.
 *
 * ## The first row
 *
 * "The whole conversation" is not a turn and is not sent as one. A fork with no
 * index copies everything, including whatever came after the last prompt, and
 * keeps the parent's last-turn summary and recap — a partial fork clears all
 * three, because the work they describe may not be in the child
 * (`storage/jsonl/copy.rs:573-589`). So the two are genuinely different calls
 * and the whole-conversation one is the one this button used to make.
 */
export function ForkPicker(props: {
  gateway: Gateway;
  onClose: () => void;
  /** The child's id, once the agent has named it. */
  onForked: (sessionId: string) => void;
}): JSX.Element {
  const [points, setPoints] = createSignal<RewindPoint[] | null>(null);
  const [running, setRunning] = createSignal(false);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal(0);

  void props.gateway.rewindPoints().then(setPoints);

  /**
   * Somebody else rewound the session while this was open.
   *
   * A fork of a timeline that has just been cut is not the fork that was asked
   * for: the indices in these rows name turns the session no longer has, and
   * picking one would branch from a different turn than the one under the
   * cursor. Closing is the same answer the rewind picker gives, and for the
   * same reason.
   */
  createEffect(
    on(
      () => props.gateway.rewoundElsewhere(),
      () => props.onClose(),
      { defer: true },
    ),
  );

  /** The whole conversation, then every turn newest first. `null` is the first. */
  const rows = (): (RewindPoint | null)[] => [null, ...(points() ?? [])];
  const at = (): number => Math.min(selected(), rows().length - 1);

  const branch = async (point: RewindPoint | null): Promise<void> => {
    setRunning(true);
    const forked = await props.gateway.fork(point?.promptIndex);
    setRunning(false);
    if (forked === null) {
      setFailure("The agent named no new session.");
      return;
    }
    props.onForked(forked);
    props.onClose();
  };

  let card!: HTMLElement;
  onMount(() => {
    // One step, so Escape leaves outright: there is no confirm to step back to.
    trapFocus(card, () => {
      if (!running()) props.onClose();
    });
    focusInto(card, ".fork-close");
  });

  const onKey = (event: KeyboardEvent): void => {
    if (running()) return;
    const count = rows().length;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      setSelected((((at() + step) % count) + count) % count);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      void branch(rows()[at()] ?? null);
    }
  };

  return (
    <div class="picker-scrim" onClick={() => (running() ? undefined : props.onClose())}>
      <section
        class="picker fork-picker"
        ref={card}
        role="dialog"
        aria-modal="true"
        aria-label="Fork the conversation"
        onClick={(event) => event.stopPropagation()}
        onKeyDown={onKey}
      >
        <header class="picker-header">
          <h2 class="picker-title">Fork from which turn?</h2>
          <button
            class="picker-close fork-close"
            type="button"
            disabled={running()}
            onClick={() => props.onClose()}
          >
            Cancel
          </button>
        </header>

        {/* Where the inclusive cut is stated, once, rather than repeated on
            every row. Which turn a fork keeps is the one thing a person cannot
            work out from a list of turns, and it is the opposite of what the
            rewind picker beside it means by the same row. */}
        <p class="fork-note">
          The new session gets this conversation up to and including the turn you
          pick. This one is left exactly as it is.
        </p>

        <Switch>
          <Match when={failure() !== null}>
            <div class="fork-state fork-failed">
              <p class="fork-failed-title">Fork failed</p>
              <p class="fork-failed-message">{failure()}</p>
              <button class="fork-dismiss" type="button" onClick={() => setFailure(null)}>
                Dismiss
              </button>
            </div>
          </Match>
          <Match when={running()}>
            <p class="fork-state">Forking…</p>
          </Match>
          <Match when={points() === null}>
            <p class="fork-state">Loading turns…</p>
          </Match>
          <Match when={true}>
            <div class="picker-list" role="listbox">
              <For each={rows()}>
                {(point, index) => (
                  <Row
                    point={point}
                    selected={index() === at()}
                    onPick={() => {
                      setSelected(index());
                      void branch(point);
                    }}
                  />
                )}
              </For>
            </div>
          </Match>
        </Switch>
      </section>
    </div>
  );
}

/** What the whole-conversation row says; it is not a turn and has no preview. */
export const WHOLE_CONVERSATION = "The whole conversation";

function Row(props: {
  point: RewindPoint | null;
  selected: boolean;
  onPick: () => void;
}): JSX.Element {
  let element: HTMLButtonElement | undefined;
  createEffect(() => {
    if (props.selected) element?.scrollIntoView({ block: "nearest" });
  });

  return (
    <button
      ref={element}
      class="picker-row fork-row"
      classList={{ selected: props.selected, "fork-row-whole": props.point === null }}
      type="button"
      role="option"
      aria-selected={props.selected}
      onClick={props.onPick}
    >
      <span class="picker-mark" aria-hidden="true">
        {props.selected ? PROMPT_ARROW : ""}
      </span>
      <span class="fork-row-preview">
        {props.point === null ? WHOLE_CONVERSATION : rewindLabel(props.point)}
      </span>
    </button>
  );
}
