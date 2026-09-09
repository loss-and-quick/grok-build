import {
  For,
  Match,
  Show,
  Switch,
  createEffect,
  createSignal,
  on,
  onMount,
  type JSX,
} from "solid-js";

import { focusInto, trapFocus } from "../focus.ts";
import type { Gateway } from "../gateway.ts";
import { PROMPT_ARROW } from "../glyphs.ts";
import { rewindConfirmTitle, rewindLabel, type RewindPoint } from "../rewind.ts";

/**
 * What the pager's `/rewind` opens, minus the phase a browser cannot offer.
 *
 * The terminal's overlay has six phases (`views/rewind.rs`, `RewindPhase`):
 * loading, the picker, an offer to cancel a running turn, the confirm, the
 * execute, and the error. Five of them are here, in the terminal's own words.
 * The sixth is `CancelOffer`, and it is missing because the thing it offers is:
 * this client cannot cancel a turn — `session/cancel` is on the wire and no
 * path here sends it — so the honest form of that phase is not to open at all
 * while a turn is running, which is what the button does.
 *
 * ## Why it always asks
 *
 * The terminal's confirm is a setting, `confirm_before_rewind`, and its
 * description says turning it off makes a rewind happen "immediately when you
 * pick a turn". What that setting removes is the *second* half of a gesture
 * that already had two: the cursor has to be walked onto the row before Enter
 * means anything. A click is one whole gesture with no first half to lean on,
 * so removing the confirm here would not restore the terminal's behaviour, it
 * would make discarding a conversation a single click on a list.
 *
 * That is the same reason the stop button in the rail arms on the first press
 * and sends on the second, and it is the same trade: the browser owes a step
 * the terminal got for free. So the row is a pick and the confirm is the act,
 * whatever the setting says — and this client could not turn the setting off
 * anyway, since it does not write settings.
 */
export function RewindPicker(props: {
  gateway: Gateway;
  onClose: () => void;
  /** The rewound-away prompt's own text, for the composer to take back. */
  onRewound: (promptText: string | null) => void;
}): JSX.Element {
  const [points, setPoints] = createSignal<RewindPoint[] | null>(null);
  const [chosen, setChosen] = createSignal<RewindPoint | null>(null);
  const [running, setRunning] = createSignal(false);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal(0);

  void props.gateway.rewindPoints().then(setPoints);

  /**
   * Somebody else rewound the session while this was open, so it steps aside.
   *
   * Every row and the confirm behind it are addressed by prompt index, and
   * those indices now name a timeline the session does not have: the turn under
   * "Rewind conversation to …?" may be a different turn or no turn at all.
   * Re-reading the list in place would be worse than closing, because it would
   * silently swap what a half-finished gesture was pointing at.
   *
   * This client's own rewind never lands here — the gateway skips the marker
   * while it is the one rewinding, and this dialog closes on its answer — so
   * the only thing that reaches this is a peer. Deferred, so opening is not
   * itself an event.
   */
  createEffect(
    on(
      () => props.gateway.rewoundElsewhere(),
      () => props.onClose(),
      { defer: true },
    ),
  );

  const rows = (): RewindPoint[] => points() ?? [];
  const at = (): number => Math.min(selected(), Math.max(rows().length - 1, 0));

  const execute = async (point: RewindPoint): Promise<void> => {
    setRunning(true);
    const result = await props.gateway.rewind(point.promptIndex);
    setRunning(false);
    if (result === null) {
      setFailure("The agent could not be reached.");
      return;
    }
    if (!result.success) {
      setFailure(result.error ?? "The agent gave no reason.");
      return;
    }
    props.onRewound(result.promptText);
    props.onClose();
  };

  let card!: HTMLElement;
  onMount(() => {
    trapFocus(card, () => {
      // Escape steps back out of the confirm before it closes the dialog, the
      // way the terminal's own Escape does.
      if (chosen() !== null && !running()) {
        setChosen(null);
        return;
      }
      if (!running()) props.onClose();
    });
    focusInto(card, ".rewind-close");
  });

  const onKey = (event: KeyboardEvent): void => {
    if (chosen() !== null || running()) return;
    const count = rows().length;
    if (count === 0) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      setSelected((((at() + step) % count) + count) % count);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const row = rows()[at()];
      if (row) setChosen(row);
    }
  };

  return (
    <div class="picker-scrim" onClick={() => (running() ? undefined : props.onClose())}>
      <section
        class="picker rewind-picker"
        ref={card}
        role="dialog"
        aria-modal="true"
        aria-label="Rewind the conversation"
        onClick={(event) => event.stopPropagation()}
        onKeyDown={onKey}
      >
        <header class="picker-header">
          {/* The pager's own title for the same list. */}
          <h2 class="picker-title">Rewind to which turn?</h2>
          <button
            class="picker-close rewind-close"
            type="button"
            disabled={running()}
            onClick={() => props.onClose()}
          >
            Cancel
          </button>
        </header>

        <Switch>
          <Match when={failure() !== null}>
            <div class="rewind-state rewind-failed">
              <p class="rewind-failed-title">Rewind failed</p>
              <p class="rewind-failed-message">{failure()}</p>
              <button class="rewind-dismiss" type="button" onClick={() => setFailure(null)}>
                Dismiss
              </button>
            </div>
          </Match>
          <Match when={running()}>
            <p class="rewind-state">Rewinding…</p>
          </Match>
          <Match when={points() === null}>
            <p class="rewind-state">Loading rewind points…</p>
          </Match>
          <Match when={rows().length === 0}>
            {/* The terminal's own words for an empty list. */}
            <p class="rewind-state">No undoable prompts.</p>
          </Match>
          <Match when={chosen()}>
            {(point) => (
              <div class="rewind-confirm">
                <p class="rewind-question">{rewindConfirmTitle(point())}</p>
                {/* The two things the rows cannot say, and both are about
                    reach. Files are the mode this client sends
                    (`conversation_only`), so a turn's edits outlive the turn.
                    The other clients used to be a warning: the agent wrote its
                    `rewind_marker` to the log and sent it to nobody, so a
                    terminal on this session went on drawing the discarded turns
                    until it loaded the session again. The marker is now sent as
                    well as persisted (`acp_session_impl/rewind.rs`), so the
                    sentence had to change with it — a dialog that still warned
                    about that would be the one thing on this screen that is
                    simply false. */}
                <p class="rewind-warning">
                  This discards the turn and everything after it, in every client
                  attached to this session — a terminal, another tab. Files are
                  left alone.
                </p>
                <div class="rewind-actions">
                  <button
                    class="rewind-go"
                    type="button"
                    onClick={() => void execute(point())}
                  >
                    Rewind
                  </button>
                  <button class="rewind-back" type="button" onClick={() => setChosen(null)}>
                    No
                  </button>
                </div>
              </div>
            )}
          </Match>
          <Match when={rows().length > 0}>
            <div class="picker-list" role="listbox">
              <For each={rows()}>
                {(point, index) => (
                  <Row
                    point={point}
                    selected={index() === at()}
                    onPick={() => {
                      setSelected(index());
                      setChosen(point);
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

function Row(props: { point: RewindPoint; selected: boolean; onPick: () => void }): JSX.Element {
  let element: HTMLButtonElement | undefined;
  createEffect(() => {
    if (props.selected) element?.scrollIntoView({ block: "nearest" });
  });

  return (
    <button
      ref={element}
      class="picker-row rewind-row"
      classList={{ selected: props.selected }}
      type="button"
      role="option"
      aria-selected={props.selected}
      onClick={props.onPick}
    >
      <span class="picker-mark" aria-hidden="true">
        {props.selected ? PROMPT_ARROW : ""}
      </span>
      <span class="rewind-row-preview">{rewindLabel(props.point)}</span>
      {/* Three fields ride every point and the terminal draws none of them.
          This one is drawn because the mode this client sends leaves files
          alone: a turn that changed files is a turn whose changes stay, and
          that is worth knowing before picking it rather than after. */}
      <Show when={props.point.hasFileChanges}>
        <span class="rewind-row-files">edited files</span>
      </Show>
    </button>
  );
}
