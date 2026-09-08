import type { PanelBlock } from "@grok-build/plugin/generated/PanelBlock.ts";
import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";
import {
  For,
  Index,
  Match,
  Switch,
  createEffect,
  createMemo,
  onCleanup,
  untrack,
  type JSX,
} from "solid-js";

import { toneColor, type PanelAction } from "../panel.ts";
import { Markdown } from "./Markdown.tsx";

type BlockOf<K extends PanelBlock["kind"]> = Extract<PanelBlock, { kind: K }>;

/**
 * The block, if it is of this kind.
 *
 * `<Index>` hands the block through an accessor, and TypeScript cannot narrow a
 * function call the way it narrowed the plain value `<For>` used to pass. This
 * puts the narrowing back where `<Match>` can carry it, so each arm still reads
 * only the fields its own variant has.
 */
function asKind<K extends PanelBlock["kind"]>(kind: K, block: PanelBlock): BlockOf<K> | null {
  return block.kind === kind ? (block as BlockOf<K>) : null;
}

/**
 * The live text of every field, by input id.
 *
 * This is the browser's `LineEditor` map: the pager keeps one editor per input
 * block inside its own `PanelState` precisely so that a plugin re-publishing
 * its panel — "on every change (a status tick, a timer)" — cannot rebuild what
 * a person is halfway through typing (`pager/src/views/plugin_panel.rs:3-7`).
 *
 * Keyed by id rather than by position, for the same reason the pager keys it
 * that way: a re-publish that inserts a block above a field must not count as a
 * different field.
 */
interface Fields {
  /** Adopt a mounted element for `id`, restoring what was typed into it. */
  adopt(id: string, field: HTMLInputElement, published: string | null): void;
  /** Remember an edit, so it survives an element this component has to rebuild. */
  record(id: string, field: HTMLInputElement): void;
  /** Drop a field the panel no longer draws, so nothing collects it again. */
  forget(id: string): void;
  /** What a button press carries back, from the fields that are on screen now. */
  collect(): Record<string, string>;
}

function createFields(): Fields {
  const mounted = new Map<string, HTMLInputElement>();
  const typed = new Map<string, string>();
  return {
    adopt(id, field, published) {
      // The published value seeds a *new* field and is discarded for one this
      // panel already has — the pager's headline property, tested there as
      // `merge_reuses_editor_and_discards_new_value`. A plugin that repaints
      // while someone types must not put its own idea of the value back.
      field.value = typed.get(id) ?? published ?? "";
      typed.set(id, field.value);
      mounted.set(id, field);
    },
    record(id, field) {
      typed.set(id, field.value);
    },
    forget(id) {
      mounted.delete(id);
      typed.delete(id);
    },
    collect() {
      return Object.fromEntries([...mounted].map(([id, field]) => [id, field.value]));
    },
  };
}

/**
 * One plugin panel.
 *
 * `<Switch>` over `block.kind` is the anti-divergence mechanism applied to the
 * client: `PanelBlock` is a generated tagged union, each `<Match>` narrows to
 * one arm, and the `UnknownBlock` fallback means a new variant in
 * `sdk/plugin/src/generated/PanelBlock.ts` fails to compile against
 * `PANEL_BLOCK_KINDS` instead of rendering as nothing.
 *
 * The block list is walked with `<Index>`, not `<For>`. `<For>` keys by
 * reference, and a re-published panel is a fresh array of fresh objects, so
 * every block's DOM — the `<input>` and its caret included — was torn down and
 * rebuilt on each of a plugin's status ticks. `<Index>` keeps the node at a
 * position and moves the data through it, which is the browser's equivalent of
 * what `PanelState::merge` does in the terminal.
 */
export function Panel(props: {
  plugin: string;
  viewModel: PanelViewModel;
  onAction: (action: PanelAction) => void;
}): JSX.Element {
  const fields = createFields();

  return (
    <section class="grok-panel">
      <header class="grok-panel-header">
        <span class="grok-panel-title">{props.viewModel.title}</span>
        <span class="grok-panel-source">{props.plugin}</span>
      </header>
      <div class="grok-panel-body">
        <Index each={props.viewModel.blocks}>
          {(block) => (
            <Switch fallback={<UnknownBlock kind={block().kind} />}>
              <Match when={asKind("status", block())}>
                {(b) => (
                  <div class="grok-panel-status">
                    <For each={b().items}>
                      {(item) => (
                        <div class="grok-panel-chip">
                          <span class="grok-panel-chip-label">{item.label}</span>
                          <span class="grok-panel-chip-value" style={{ color: toneColor(item.tone) }}>
                            {item.value}
                          </span>
                        </div>
                      )}
                    </For>
                  </div>
                )}
              </Match>

              <Match when={asKind("markdown", block())}>
                {(b) => (
                  <div class="grok-panel-markdown">
                    <Markdown text={b().text} />
                  </div>
                )}
              </Match>

              <Match when={asKind("table", block())}>
                {(b) => (
                  <div class="grok-panel-table-wrap">
                    <table class="grok-panel-table">
                      <thead>
                        <tr>
                          <For each={b().columns}>{(column) => <th>{column}</th>}</For>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={b().rows}>
                          {(row) => (
                            <tr
                              class={b().selectable ? "grok-selectable" : undefined}
                              tabIndex={b().selectable ? 0 : undefined}
                            >
                              <For each={row}>{(cell) => <td>{cell}</td>}</For>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                )}
              </Match>

              <Match when={asKind("input", block())}>
                {(b) => <Field block={b()} fields={fields} />}
              </Match>

              <Match when={asKind("actions", block())}>
                {(b) => (
                  <div class="grok-panel-actions">
                    <For each={b().buttons}>
                      {(button) => (
                        <button
                          class="grok-panel-button"
                          type="button"
                          // `key` is the pager's single-character keybind while the
                          // panel is focused. Shown, not bound: a browser page has a
                          // focused text field most of the time, and stealing a letter
                          // from it would be worse than making the user click.
                          title={button.key ? `Keybind in the terminal: ${button.key}` : undefined}
                          onClick={() =>
                            props.onAction({
                              panelId: props.viewModel.id,
                              buttonId: button.id,
                              inputs: fields.collect(),
                            })
                          }
                        >
                          {button.label}
                        </button>
                      )}
                    </For>
                  </div>
                )}
              </Match>
            </Switch>
          )}
        </Index>
      </div>
    </section>
  );
}

/**
 * One `input` block.
 *
 * Registration follows the block's **id**, and is undone when that id goes
 * away. Solid calls a `ref` once, at creation, and never on removal, so a map
 * filled from one would only ever grow: a panel that dropped its code box after
 * a successful exchange went on posting the code back with every later press,
 * and a field whose id moved delivered its value under the name of a field that
 * no longer existed. An effect keyed on the id both registers and cleans up,
 * which is what makes the collector describe the panel that is on screen.
 *
 * The value seeds a field once, as the id arrives, rather than being bound to
 * the block. A binding would re-apply on every re-publish, which is the
 * behaviour the pager deliberately does not have: it keeps the live editor for
 * an id it already knows and ignores the value that came with it.
 */
function Field(props: { block: BlockOf<"input">; fields: Fields }): JSX.Element {
  let field!: HTMLInputElement;
  // Through a memo, so the effect wakes when the *id* changes and not merely
  // because a re-publish handed down a new object carrying the same id — which
  // is every status tick, and would re-seed the field on each one.
  const id = createMemo(() => props.block.id);
  createEffect(() => {
    const key = id();
    props.fields.adopt(key, field, untrack(() => props.block.value));
    onCleanup(() => props.fields.forget(key));
  });
  return (
    <label class="grok-panel-input">
      <span class="grok-panel-input-label">{props.block.label}</span>
      <input
        type={props.block.secret ? "password" : "text"}
        placeholder={props.block.placeholder ?? undefined}
        ref={field}
        onInput={(event) => props.fields.record(props.block.id, event.currentTarget)}
      />
    </label>
  );
}

/**
 * A block kind this build does not draw.
 *
 * Unreachable while `PANEL_BLOCK_KINDS` compiles, and visible rather than
 * silent if a newer agent ever sends one to an older page: a plugin author
 * should see that their block did not render, not wonder where it went.
 */
function UnknownBlock(props: { kind: string }): JSX.Element {
  return <div class="grok-panel-unknown">unrenderable block: {props.kind}</div>;
}
