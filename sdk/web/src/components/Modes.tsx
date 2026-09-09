import { For, Show, createSignal, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";
import { GLYPH_WARNING, DOT_FILLED, DOT_HOLLOW } from "../glyphs.ts";
import {
  PERMISSION_MODES,
  SESSION_MODES,
  permissionModeOf,
  type PermissionMode,
  type PermissionModeChoice,
} from "../modes.ts";
import { Overlay } from "./Overlay.tsx";

/**
 * How this session runs: what the agent may do, and how much it decides alone.
 *
 * Beside the composer because both are properties of the *next* thing the agent
 * will do, which is the thing being typed — the pager reaches the same two with
 * Shift+Tab from the prompt for the same reason.
 *
 * The two rows are genuinely two mechanisms and are not merged into one scale.
 * The left is an ACP session mode set with `session/set_mode`; the right is the
 * permission mode, which rides `_x.ai/yolo_mode_changed`. `modes.ts` argues why
 * pretending they are one control would misreport both.
 */
export function Modes(props: { gateway: Gateway }): JSX.Element {
  const [confirming, setConfirming] = createSignal<PermissionModeChoice | null>(null);
  const [chosen, setChosen] = createSignal<PermissionMode | null>(null);

  // What the roster can actually tell us, plus what this browser has set since.
  // The roster only carries `yolo`, so "ask" and "auto" are indistinguishable
  // from a fresh attach — and a control that guessed would promise permission
  // requests that never come.
  const permissionMode = (): PermissionMode | null =>
    chosen() ?? permissionModeOf(props.gateway.attached()?.entry.yolo);

  const apply = (choice: PermissionModeChoice): void => {
    if (choice.confirm) {
      setConfirming(choice);
      return;
    }
    setChosen(choice.id);
    props.gateway.setPermissionMode(choice.id);
  };

  return (
    <div class="modes">
      <div class="modes-group" role="group" aria-label="Session mode">
        <For each={SESSION_MODES}>
          {(mode) => (
            <button
              class="mode"
              type="button"
              title={mode.description}
              aria-pressed={props.gateway.sessionMode() === mode.id}
              onClick={() => void props.gateway.setSessionMode(mode.id)}
            >
              <span class="mode-marker" aria-hidden="true">
                {props.gateway.sessionMode() === mode.id ? DOT_FILLED : DOT_HOLLOW}
              </span>
              {mode.label}
            </button>
          )}
        </For>
      </div>

      <div class="modes-group" role="group" aria-label="How much the agent decides alone">
        <For each={PERMISSION_MODES}>
          {(choice) => (
            <button
              class="mode"
              classList={{ "mode-unasking": choice.confirm }}
              type="button"
              title={choice.description}
              aria-pressed={permissionMode() === choice.id}
              onClick={() => apply(choice)}
            >
              <span class="mode-marker" aria-hidden="true">
                {permissionMode() === choice.id ? DOT_FILLED : DOT_HOLLOW}
              </span>
              {choice.label}
            </button>
          )}
        </For>
      </div>

      {/* The one mode change that is asked about, and it is asked about for the
          same reason a permission request is a modal: this is the switch that
          stops the agent asking at all. Every other mode here is undone by
          picking another one; the turns that ran unasked while this was on are
          not. */}
      <Show when={confirming()}>
        {(choice) => (
          <Overlay label="Let the agent act without asking?" onClose={() => setConfirming(null)}>
            <p class="mode-consequence">
              <span class="mode-consequence-mark" aria-hidden="true">
                {GLYPH_WARNING}
              </span>
              {choice().description} It stays that way until you change it back, and it
              covers every session this browser has open on this agent — the agent applies
              the change per client, not per session.
            </p>
            <div class="mode-confirm-actions">
              <button
                class="mode-confirm"
                type="button"
                onClick={() => {
                  setChosen(choice().id);
                  props.gateway.setPermissionMode(choice().id);
                  setConfirming(null);
                }}
              >
                Stop asking me
              </button>
              <button class="mode-cancel" type="button" onClick={() => setConfirming(null)}>
                Keep asking
              </button>
            </div>
          </Overlay>
        )}
      </Show>
    </div>
  );
}
