import { For, type JSX } from "solid-js";

import { blendToward, createTick, waitingBrightness } from "../animation.ts";
import type { PendingPermission } from "../gateway.ts";
import { BULLET, DOT_FILLED, DOT_HOLLOW } from "../glyphs.ts";

/**
 * A shared permission modal.
 *
 * The options are drawn from the `options` array as sent, never a hardcoded id
 * list: which options exist depends on the tool and on the client type the
 * leader registered, and a client that assumes them answers with an id the
 * agent does not know.
 *
 * The diamond pulses on the pager's "waiting on you" curve — `0.3 + sin²(…)·0.7`
 * — which the pager uses to say *paused on you* rather than *still working*.
 * That distinction is the reason it is a pulse and not the running wave.
 */
export function PermissionCard(props: { pending: PendingPermission }): JSX.Element {
  const tick = createTick();

  return (
    <div class="permission">
      <div class="permission-title">
        <span
          class="permission-diamond"
          aria-hidden="true"
          style={{
            color: blendToward(
              "var(--grok-bg-base)",
              "var(--grok-accent-user)",
              waitingBrightness(tick()),
            ),
          }}
        >
          {BULLET}
        </span>
        {props.pending.title}
      </div>
      <div class="permission-options">
        <For each={props.pending.request.options ?? []}>
          {(option, index) => (
            <button
              class={`permission-option permission-${option.kind}`}
              type="button"
              onClick={() =>
                props.pending.answer({ outcome: { outcome: "selected", optionId: option.optionId } })
              }
            >
              {/* The pager numbers the options and marks the cursor with a
                  filled dot; nothing is focused here until the user moves, so
                  the first option carries the marker the same way. */}
              <span class="permission-key" aria-hidden="true">
                {index() + 1}
              </span>
              <span class="permission-marker" aria-hidden="true">
                ({index() === 0 ? DOT_FILLED : DOT_HOLLOW})
              </span>
              {option.name}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
