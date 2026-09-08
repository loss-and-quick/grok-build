import { For, Show, createSignal, onCleanup, onMount, type JSX } from "solid-js";

import {
  STATE_ROLE,
  describeSeen,
  hostOf,
  type Instance,
  type InstanceState,
} from "../instances.ts";
import { cssVarName } from "../theme.ts";

/**
 * The machines this browser knows, and which one it is looking at.
 *
 * A menu rather than a tree. An instance is not a level above the roster — the
 * roster *is* one socket's answer to `x.ai/sessions/list`, and `roster.replace`
 * wipes it — so drawing instances as expandable nodes with directories under
 * them would mean holding several rosters at once, which means several live
 * sockets, which means several logins. This is a context switch above the
 * roster instead: one line at the top of the navigator, and everything below it
 * changes.
 *
 * Nothing here connects on its own. Switching is a press, because on this wire
 * connecting *is* signing in.
 */
export function InstanceMenu(props: {
  instances: readonly Instance[];
  currentId: string | null;
  defaultId: string | null;
  /** The state of the current instance; every other row is not connected. */
  state: InstanceState;
  onSwitch: (instance: Instance) => void;
  onRename: (id: string, label: string) => void;
  onForget: (id: string) => void;
  onMakeDefault: (id: string) => void;
  onAdd: () => void;
  onClose: () => void;
  /** Injected so a test can date a row without waiting for the clock. */
  now?: number;
}): JSX.Element {
  const [renaming, setRenaming] = createSignal<string | null>(null);
  let card!: HTMLDivElement;

  // Escape and a click elsewhere close it. Not a focus trap and not
  // `aria-modal`: the page behind a menu is not hidden, and claiming it is
  // would be the promise `focus.ts` exists to stop this client making twice.
  onMount(() => {
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") props.onClose();
    };
    const onDown = (event: MouseEvent): void => {
      if (event.target instanceof Node && !card.contains(event.target)) props.onClose();
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    onCleanup(() => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onDown);
    });
  });

  const stateOf = (instance: Instance): InstanceState => {
    if (instance.id === props.currentId) return props.state;
    return instance.agentId ? "away" : "never";
  };

  const detail = (instance: Instance): string => {
    const parts: string[] = [];
    if (instance.sessionCount !== undefined) {
      parts.push(instance.sessionCount === 1 ? "1 session" : `${instance.sessionCount} sessions`);
    }
    parts.push(describeSeen(instance.lastSeenUnixMs, props.now ?? Date.now()));
    if (instance.agentVersion) parts.push(instance.agentVersion);
    return parts.join(" · ");
  };

  return (
    <div class="instances" role="menu" aria-label="Instances" ref={card}>
      <For each={props.instances}>
        {(instance) => (
          <div
            class="instance"
            classList={{ current: instance.id === props.currentId }}
            aria-current={instance.id === props.currentId ? "true" : undefined}
          >
            <span
              class="instance-dot"
              aria-hidden="true"
              style={{ background: `var(${cssVarName(STATE_ROLE[stateOf(instance)])})` }}
            />
            <Show
              when={renaming() === instance.id}
              fallback={
                <button
                  class="instance-name"
                  type="button"
                  role="menuitem"
                  onClick={() => props.onSwitch(instance)}
                >
                  {instance.label}
                </button>
              }
            >
              {/* Renaming happens in place: the label is a client-side name for
                  a machine, so there is nothing to ask an agent about and
                  nothing to fail. */}
              <input
                class="instance-rename"
                value={instance.label}
                autofocus
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    props.onRename(instance.id, event.currentTarget.value.trim());
                    setRenaming(null);
                  }
                  if (event.key === "Escape") {
                    event.stopPropagation();
                    setRenaming(null);
                  }
                }}
                onBlur={(event) => {
                  props.onRename(instance.id, event.currentTarget.value.trim());
                  setRenaming(null);
                }}
              />
            </Show>
            {/* Every address known to reach this machine, not just the one in
                use: two of them here is the merge having happened, and the only
                place a person can see that it did. */}
            <span class="instance-hosts">
              {instance.addresses.map((address) => hostOf(address)).join(" · ")}
            </span>
            <span class="instance-detail">{detail(instance)}</span>
            <div class="instance-actions">
              <button
                class="instance-rename-button"
                type="button"
                role="menuitem"
                onClick={() => setRenaming(instance.id)}
              >
                Rename
              </button>
              {/* Which instance a *new* tab opens on, kept apart from the one
                  this tab is looking at. Without the distinction, looking at
                  another machine once quietly decides where every tab opened
                  afterwards signs in. */}
              <Show
                when={instance.id !== props.defaultId}
                fallback={<span class="instance-default">opens new tabs</span>}
              >
                <button
                  class="instance-default-button"
                  type="button"
                  role="menuitem"
                  onClick={() => props.onMakeDefault(instance.id)}
                >
                  Open new tabs here
                </button>
              </Show>
              <button
                class="instance-forget"
                type="button"
                role="menuitem"
                onClick={() => props.onForget(instance.id)}
              >
                Forget
              </button>
            </div>
          </div>
        )}
      </For>
      <Show when={props.instances.length === 0}>
        <p class="empty">
          No gateway yet. A `grok agent gateway` prints its own address and secret when it starts.
        </p>
      </Show>
      <div class="instance-footer">
        <button class="instance-add" type="button" role="menuitem" onClick={() => props.onAdd()}>
          Add instance…
        </button>
      </div>
    </div>
  );
}
