import { For, Show, createEffect, createSignal, onCleanup, onMount, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";
import { CHEVRON, CHEVRON_LEFT, PROMPT_ARROW } from "../glyphs.ts";
import {
  currentEffort,
  effortRows,
  filterRows,
  modelById,
  modelRows,
  type PickerRow,
  type SessionModelState,
} from "../models.ts";

/**
 * Which model the attached session is on, and the way to change it.
 *
 * The catalog was already arriving: `session/load` answers with `models`, and
 * this client used to throw the whole reply away. So there is no wire change
 * behind this screen, no Rust change, and no new vocabulary — see `models.ts`.
 */
export function ModelPicker(props: { gateway: Gateway }): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const state = (): SessionModelState | null => props.gateway.models();
  const current = () => {
    const at = state();
    return at ? modelById(at, at.currentModelId) : undefined;
  };

  return (
    <Show when={state()}>
      {(catalog) => (
        <>
          {/* The header line the terminal keeps in its status bar: the model,
              and its effort when it has one. */}
          <button
            class="model-current"
            type="button"
            title="Switch the model for this session"
            onClick={() => setOpen(true)}
          >
            <span class="model-name">{current()?.name ?? catalog().currentModelId}</span>
            <Show when={currentEffort(current())}>
              {(effort) => <span class="model-effort">{effort()}</span>}
            </Show>
          </button>
          <Show when={open()}>
            <ModelDialog
              state={catalog()}
              onClose={() => setOpen(false)}
              onPick={(modelId, effort) => {
                setOpen(false);
                void props.gateway.setModel(modelId, effort);
              }}
            />
          </Show>
        </>
      )}
    </Show>
  );
}

/**
 * The two-phase list the terminal opens on Ctrl+M.
 *
 * Both phases are the pager's, row for row: `build_model_items` and
 * `build_effort_items` (`pager/src/slash/commands/model.rs:125-176`), filtered
 * by the substring match its `ArgPicker` actually applies
 * (`pager/src/app/modals.rs:642-652`). A model that supports reasoning effort
 * opens the second phase instead of applying, which is the terminal's decision
 * about models rather than about text: `arg_items_look_like_effort_phase` and
 * the `chains_to_effort` branch at `modals.rs:684-706`.
 *
 * What that decision costs is worth stating: chosen from this dialog, a
 * reasoning model is a **session** switch and is not remembered, because
 * `/model <name> <effort>` is not persisted. The terminal's other route to
 * remembering one is typing `/model <name>` into the composer, which is a pager
 * command — the shell never advertises it, so a browser cannot dispatch it. The
 * wire's own home for that is the `default_model` row of the settings catalog,
 * `surface: any` and already drawn here, which this client still renders
 * read-only.
 *
 * Escape steps back out of the effort phase before it closes the dialog, the way
 * `try_arg_picker_step_back_from_effort` does (`modals.rs:32-38`).
 */
function ModelDialog(props: {
  state: SessionModelState;
  onClose: () => void;
  onPick: (modelId: string, effort?: string) => void;
}): JSX.Element {
  // The model whose effort levels are on screen; `null` in the first phase.
  const [chosen, setChosen] = createSignal<string | null>(null);
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal(0);

  const all = (): PickerRow[] => {
    const model = chosen();
    return model === null ? modelRows(props.state) : effortRows(props.state, model);
  };
  const rows = (): PickerRow[] => filterRows(all(), query());
  const at = (): number => Math.min(selected(), Math.max(rows().length - 1, 0));

  const take = (row: PickerRow): void => {
    const model = chosen();
    if (model !== null) {
      props.onPick(model, row.id);
      return;
    }
    if (row.chainsToEffort) {
      setChosen(row.id);
      setQuery("");
      setSelected(0);
      return;
    }
    props.onPick(row.id);
  };

  /** Escape, and the back control: out of the effort phase, then out of the dialog. */
  const back = (): void => {
    if (chosen() === null) {
      props.onClose();
      return;
    }
    setChosen(null);
    setQuery("");
    setSelected(0);
  };

  let card!: HTMLElement;
  let opener: HTMLElement | null = null;

  const focusable = (): HTMLElement[] => [
    ...card.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
    ),
  ];

  onMount(() => {
    opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    card.querySelector<HTMLInputElement>(".model-filter")?.focus();

    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        event.preventDefault();
        back();
        return;
      }
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const count = rows().length;
        if (count === 0) return;
        const step = event.key === "ArrowDown" ? 1 : -1;
        // Wraps at both ends, like `SlashController::move_selection`.
        setSelected((((at() + step) % count) + count) % count);
        return;
      }
      if (event.key === "Enter") {
        event.preventDefault();
        const row = rows()[at()];
        if (row) take(row);
        return;
      }
      if (event.key !== "Tab") return;
      // The same promise `aria-modal` makes elsewhere in this client, kept the
      // same way: focus does not leave a dialog that says it is modal.
      const items = focusable();
      const first = items[0];
      const last = items[items.length - 1];
      if (!first || !last) {
        event.preventDefault();
        return;
      }
      const active = document.activeElement;
      if (!(active instanceof HTMLElement) || !card.contains(active)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
        return;
      }
      if (event.shiftKey && active === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      document.removeEventListener("keydown", onKey);
      if (opener?.isConnected) opener.focus();
    });
  });

  const chosenName = (): string => {
    const model = chosen();
    if (model === null) return "";
    return modelById(props.state, model)?.name ?? model;
  };

  return (
    <div class="picker-scrim" onClick={() => props.onClose()}>
      <section
        class="picker model-picker"
        ref={card}
        role="dialog"
        aria-modal="true"
        aria-label="Switch model"
        onClick={(event) => event.stopPropagation()}
      >
        <header class="picker-header">
          <h2 class="picker-title">
            <Show when={chosen() !== null} fallback="Model">
              <button class="model-back" type="button" onClick={back}>
                <span aria-hidden="true">{CHEVRON_LEFT}</span> Model
              </button>
              <span class="picker-crumb-sep" aria-hidden="true">
                {CHEVRON}
              </span>
              {chosenName()}
            </Show>
          </h2>
          <button class="picker-close" type="button" onClick={() => props.onClose()}>
            Cancel
          </button>
        </header>

        <input
          class="model-filter"
          type="text"
          spellcheck={false}
          autocomplete="off"
          aria-label="Filter"
          placeholder={chosen() === null ? "Filter models" : "Filter effort levels"}
          value={query()}
          onInput={(event) => {
            setQuery(event.currentTarget.value);
            setSelected(0);
          }}
        />

        <div class="picker-list" role="listbox">
          <For each={rows()}>
            {(row, index) => (
              <Row
                row={row}
                selected={index() === at()}
                onPick={() => {
                  setSelected(index());
                  take(row);
                }}
              />
            )}
          </For>
          <Show when={rows().length === 0}>
            <p class="picker-note">Nothing matches “{query()}”.</p>
          </Show>
        </div>

        {/* The one thing a person cannot see from the rows: which of the two
            switches they are about to make. */}
        <footer class="picker-footer">
          <p class="model-note">
            <Show
              when={chosen() === null}
              fallback="Applies to this session only; the default model is left as it is."
            >
              Switches this session and becomes the default for new ones.
            </Show>
          </p>
        </footer>
      </section>
    </div>
  );
}

function Row(props: { row: PickerRow; selected: boolean; onPick: () => void }): JSX.Element {
  let element: HTMLButtonElement | undefined;
  createEffect(() => {
    if (props.selected) element?.scrollIntoView({ block: "nearest" });
  });

  return (
    <button
      ref={element}
      class="picker-row model-row"
      classList={{ selected: props.selected }}
      type="button"
      role="option"
      aria-selected={props.selected}
      onClick={props.onPick}
    >
      <span class="picker-mark" aria-hidden="true">
        {props.selected ? PROMPT_ARROW : ""}
      </span>
      <span class="model-row-name">{props.row.display}</span>
      <span class="model-row-description">{props.row.description}</span>
      {/* The terminal says the same thing with a trailing space in the text it
          would insert; a list can just show where the row goes. */}
      <Show when={props.row.chainsToEffort}>
        <span class="model-row-more" aria-hidden="true">
          {CHEVRON}
        </span>
      </Show>
    </button>
  );
}
