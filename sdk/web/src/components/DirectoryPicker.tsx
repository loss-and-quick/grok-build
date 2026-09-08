import { For, Show, createSignal, onMount, type JSX } from "solid-js";

import {
  SEPARATOR,
  breadcrumbs,
  childOf,
  emptyReason,
  isAbsolute,
  normalizePath,
  parentOf,
  partition,
} from "../directory.ts";
import { focusInto, trapFocus } from "../focus.ts";
import type { Gateway } from "../gateway.ts";
import { CHEVRON, CHEVRON_LEFT, DISCLOSURE_CLOSED } from "../glyphs.ts";
import type { FsNode } from "../wire.ts";

/**
 * Choosing the root of a new session.
 *
 * This is the gesture the product's framing implies and the browser could not
 * previously make: "an instance is a session with a fixed working directory,
 * and the client switches between them" is only true if the client can name a
 * directory the roster has never mentioned. Before this, a browser could open a
 * session only where a terminal had already opened one.
 *
 * Nothing new is asked of the wire. `x.ai/fs/list` walks an absolute path as
 * given, `initialize` already names a directory to start from, and
 * `session/new` already takes any absolute `cwd` — the three pieces were all
 * present and only the screen was missing.
 */
export function DirectoryPicker(props: {
  gateway: Gateway;
  onOpen: (cwd: string) => void;
  onClose: () => void;
}): JSX.Element {
  const [path, setPath] = createSignal(props.gateway.agentCwd());
  const [typed, setTyped] = createSignal(props.gateway.agentCwd());
  const [nodes, setNodes] = createSignal<FsNode[]>([]);
  const [truncated, setTruncated] = createSignal(false);
  const [note, setNote] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // Directories can be clicked faster than the leader answers, and a slow reply
  // for a directory already left behind must not repaint the one on screen.
  // Every walk carries a ticket; only the newest may draw.
  let ticket = 0;

  const walk = async (to: string): Promise<void> => {
    const target = normalizePath(to);
    const mine = ++ticket;
    setPath(target);
    setTyped(target);
    setBusy(true);
    setNote("");
    try {
      const listing = await props.gateway.listDirectory(target);
      if (mine !== ticket) return;
      setNodes(listing.nodes);
      setTruncated(listing.truncated);
      // An empty page has three meanings on this wire and `fs/list` tells them
      // apart from none: an empty directory, a path that is a file, and a
      // directory the leader's user may not read. `fs/exists` separates only
      // the missing case, so the message names the ambiguity instead of
      // guessing which half it is.
      if (listing.nodes.length === 0) {
        const exists = await props.gateway.pathExists(target);
        if (mine === ticket) setNote(emptyReason(exists));
      }
    } catch (e) {
      if (mine !== ticket) return;
      setNodes([]);
      setTruncated(false);
      setNote(String(e));
    } finally {
      if (mine === ticket) setBusy(false);
    }
  };

  // The dialog's own element, which is what the focus trap holds.
  let card!: HTMLElement;

  onMount(() => {
    void walk(props.gateway.agentCwd());
    // The trap is shared with every other surface that claims `aria-modal`; see
    // `focus.ts` for why the claim and the trap are one decision.
    trapFocus(card, () => props.onClose());
    // Into the path field rather than onto the first button: typing a path is
    // what this dialog is for, and the card is announced on entry either way.
    focusInto(card, ".picker-path");
  });

  const up = (): string | null => parentOf(path());

  return (
    // The scrim closes on a click that reaches it, so clicking away is an exit;
    // the card stops the click so a click inside is not one.
    <div class="picker-scrim" onClick={() => props.onClose()}>
      <section
        class="picker"
        ref={card}
        role="dialog"
        aria-modal="true"
        aria-label="Choose a working directory"
        onClick={(event) => event.stopPropagation()}
      >
        <header class="picker-header">
          <h2 class="picker-title">New session in…</h2>
          <button class="picker-close" type="button" onClick={() => props.onClose()}>
            Cancel
          </button>
        </header>

        {/* Every ancestor is a button, so the header is also the way back up —
            the pager's own settings breadcrumbs, with its own chevron. */}
        <nav class="picker-crumbs" aria-label="Path">
          <For each={breadcrumbs(path())}>
            {(crumb, index) => (
              <>
                <Show when={index() > 0}>
                  <span class="picker-crumb-sep" aria-hidden="true">
                    {CHEVRON}
                  </span>
                </Show>
                <button class="picker-crumb" type="button" onClick={() => void walk(crumb.path)}>
                  {crumb.label}
                </button>
              </>
            )}
          </For>
        </nav>

        {/* Typing a path beats walking to it from the root, and it is the only
            way to reach a directory whose parent the leader cannot list. */}
        <form
          class="picker-jump"
          onSubmit={(event) => {
            event.preventDefault();
            void walk(typed());
          }}
        >
          <input
            class="picker-path"
            type="text"
            spellcheck={false}
            autocomplete="off"
            aria-label="Absolute path"
            value={typed()}
            onInput={(event) => setTyped(event.currentTarget.value)}
          />
          <button class="picker-go" type="submit" disabled={!isAbsolute(normalizePath(typed()))}>
            Go
          </button>
        </form>

        <div class="picker-list">
          <Show when={up()}>
            {(parent) => (
              <button class="picker-row picker-up" type="button" onClick={() => void walk(parent())}>
                <span class="picker-mark" aria-hidden="true">
                  {CHEVRON_LEFT}
                </span>
                <span class="picker-name">{parent()}</span>
              </button>
            )}
          </Show>

          <For each={partition(nodes()).directories}>
            {(node) => (
              <button
                class="picker-row picker-dir"
                type="button"
                title={node.path}
                onClick={() => void walk(childOf(path(), node.name))}
              >
                <span class="picker-mark" aria-hidden="true">
                  {DISCLOSURE_CLOSED}
                </span>
                {/* The trailing separator is what marks a directory, rather
                    than an icon this product does not have: it is the path
                    syntax itself, and it needs no legend. */}
                <span class="picker-name">
                  {node.name}
                  {SEPARATOR}
                </span>
                <Show when={node.isSymlink}>
                  <span class="picker-link">symlink</span>
                </Show>
              </button>
            )}
          </For>

          {/* Files are shown and not clickable. They are how a person
              recognizes the project they meant — a `Cargo.toml` here, a
              `package.json` there — which is the whole job of this screen. */}
          <For each={partition(nodes()).files}>
            {(node) => (
              <div class="picker-row picker-file" title={node.path}>
                <span class="picker-mark" aria-hidden="true" />
                <span class="picker-name">{node.name}</span>
              </div>
            )}
          </For>

          <Show when={note()}>
            <p class="picker-note">{note()}</p>
          </Show>
          <Show when={truncated()}>
            <p class="picker-note">
              Listing cut short by the agent's page limit; some entries are not shown.
            </p>
          </Show>
        </div>

        <footer class="picker-footer">
          <div class="picker-chosen" title={path()}>
            {path()}
          </div>
          <button
            class="picker-open"
            type="button"
            disabled={busy() || !isAbsolute(path())}
            onClick={() => props.onOpen(path())}
          >
            Open a session here
          </button>
        </footer>
      </section>
    </div>
  );
}
