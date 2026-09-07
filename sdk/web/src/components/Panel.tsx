import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";
import { For, Match, Switch, type JSX } from "solid-js";

import { toneColor, type PanelAction } from "../panel.ts";
import { Markdown } from "./Markdown.tsx";

/**
 * One plugin panel.
 *
 * `<Switch>` over `block.kind` is the anti-divergence mechanism applied to the
 * client: `PanelBlock` is a generated tagged union, each `<Match>` narrows to
 * one arm, and the `assertNever` fallback means a new variant in
 * `sdk/plugin/src/generated/PanelBlock.ts` fails to compile here instead of
 * rendering as nothing.
 */
export function Panel(props: {
  plugin: string;
  viewModel: PanelViewModel;
  onAction: (action: PanelAction) => void;
}): JSX.Element {
  // Live values of every `input` block, keyed by the block's id. `PanelActionParams`
  // says a press delivers these alongside the button, which is what lets a panel
  // collect an OAuth code and submit it in one gesture.
  const inputs = new Map<string, HTMLInputElement>();
  const collect = (): Record<string, string> =>
    Object.fromEntries([...inputs].map(([id, field]) => [id, field.value]));

  return (
    <section class="grok-panel">
      <header class="grok-panel-header">
        <span class="grok-panel-title">{props.viewModel.title}</span>
        <span class="grok-panel-source">{props.plugin}</span>
      </header>
      <div class="grok-panel-body">
        <For each={props.viewModel.blocks}>
          {(block) => (
            <Switch fallback={<UnknownBlock kind={block.kind} />}>
              <Match when={block.kind === "status" ? block : null}>
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

              <Match when={block.kind === "markdown" ? block : null}>
                {(b) => (
                  <div class="grok-panel-markdown">
                    <Markdown text={b().text} />
                  </div>
                )}
              </Match>

              <Match when={block.kind === "table" ? block : null}>
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

              <Match when={block.kind === "input" ? block : null}>
                {(b) => (
                  <label class="grok-panel-input">
                    <span class="grok-panel-input-label">{b().label}</span>
                    <input
                      type={b().secret ? "password" : "text"}
                      placeholder={b().placeholder ?? undefined}
                      value={b().value ?? ""}
                      ref={(field) => inputs.set(b().id, field)}
                    />
                  </label>
                )}
              </Match>

              <Match when={block.kind === "actions" ? block : null}>
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
                              inputs: collect(),
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
        </For>
      </div>
    </section>
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
