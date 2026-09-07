import { For, type JSX } from "solid-js";

import type { PendingPermission } from "../gateway.ts";

/**
 * A shared permission modal.
 *
 * The options are drawn from the `options` array as sent, never a hardcoded id
 * list: which options exist depends on the tool and on the client type the
 * leader registered, and a client that assumes them answers with an id the
 * agent does not know.
 */
export function PermissionCard(props: { pending: PendingPermission }): JSX.Element {
  return (
    <div class="permission">
      <div class="permission-title">{props.pending.title}</div>
      <div class="permission-options">
        <For each={props.pending.request.options ?? []}>
          {(option) => (
            <button
              class={`permission-option permission-${option.kind}`}
              type="button"
              onClick={() =>
                props.pending.answer({ outcome: { outcome: "selected", optionId: option.optionId } })
              }
            >
              {option.name}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
