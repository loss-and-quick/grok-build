import { For, Show, createSignal, type JSX } from "solid-js";

import type { Gateway } from "../gateway.ts";
import { DOT_FILLED, DOT_HOLLOW } from "../glyphs.ts";
import type { NoteScope } from "../wire.ts";
import { Overlay } from "./Overlay.tsx";

/**
 * The two files the agent will append a note to (`extensions/memory.rs`, `NoteScope`).
 *
 * Both are the agent's, and neither is named by this client: there is no path
 * parameter on the wire, deliberately, so that no caller can aim a write at a
 * directory of its choosing. The workspace comes from the session.
 *
 * `/remember` in the terminal only ever writes the global one — the pager sends
 * no scope, and the field defaults to global. The second is offered here
 * because the storage layer has always had it and the wire names it rather than
 * hardcoding one, which is the shell's own stated reason for the field.
 */
const SCOPES: readonly { id: NoteScope; label: string; description: string }[] = [
  {
    id: "global",
    label: "Everywhere",
    description: "The memory the agent carries into every workspace. What /remember writes.",
  },
  {
    id: "workspace",
    label: "This workspace",
    description: "The memory kept beside this session's working directory.",
  },
];

/**
 * File a note in the agent's memory.
 *
 * ## Why it says where the note went
 *
 * The note is **appended**, never written back whole, and the handler is candid
 * about what that does and does not protect: two appends at once each land
 * whole, because the storage layer writes one buffer to an `O_APPEND` handle,
 * but an append racing a hand edit is a **lost note** — an editor saving the
 * file writes back a buffer it read before the append happened, and the note
 * goes with it. Nothing on either side can fix that, and the shell says so in
 * its own doc comment rather than pretending otherwise.
 *
 * So this does not report "saved" and close. It names the file, because the
 * file is the only place the claim can be checked, and it is checkable by the
 * one person who might also have it open in an editor.
 *
 * ## What is not done here
 *
 * `x.ai/memory/rewrite` — the LLM pass `/remember` runs before saving — is not
 * called. It is a model round trip whose result the terminal shows for review
 * in the composer before writing, and a browser offering the rewrite without
 * that review would file something the person never read. The write is the half
 * that was missing from the wire; the rewrite was already there and can be
 * added on top of a surface that exists.
 */
export function Remember(props: { gateway: Gateway; onClose: () => void }): JSX.Element {
  const [text, setText] = createSignal("");
  const [scope, setScope] = createSignal<NoteScope>("global");
  const [saving, setSaving] = createSignal(false);
  const [saved, setSaved] = createSignal<string | null>(null);
  const [failed, setFailed] = createSignal<string | null>(null);

  const save = async (): Promise<void> => {
    const note = text().trim();
    // The handler rejects an empty note with `invalid_params`; refusing here
    // keeps that from being reported to a person as a failure of the agent.
    if (note === "" || saving()) return;
    setSaving(true);
    setFailed(null);
    try {
      setSaved(await props.gateway.saveMemoryNote(note, scope()));
    } catch (e) {
      setFailed(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Overlay label="Remember" onClose={props.onClose}>
      <Show
        when={saved() === null}
        fallback={
          <div class="remember-done">
            <p class="remember-landed">Appended to</p>
            {/* The path, on its own line and selectable: this is the thing a
                person goes and looks at, and the reason it is shown at all. */}
            <p class="remember-path">{saved()}</p>
            <p class="remember-caveat">
              Notes are appended. If you have that file open in an editor, saving it there will
              write back the copy it read and this note will go with it.
            </p>
            <button type="button" class="remember-close" onClick={() => props.onClose()}>
              Done
            </button>
          </div>
        }
      >
        <div class="remember">
          <label class="remember-label" for="remember-note">
            What should the agent remember?
          </label>
          <textarea
            id="remember-note"
            class="remember-note"
            rows={5}
            value={text()}
            onInput={(event) => setText(event.currentTarget.value)}
          />
          <div class="remember-scopes" role="group" aria-label="Which memory">
            <For each={SCOPES}>
              {(choice) => (
                <button
                  type="button"
                  class="remember-scope"
                  title={choice.description}
                  aria-pressed={scope() === choice.id}
                  onClick={() => setScope(choice.id)}
                >
                  <span class="remember-scope-marker" aria-hidden="true">
                    {scope() === choice.id ? DOT_FILLED : DOT_HOLLOW}
                  </span>
                  {choice.label}
                </button>
              )}
            </For>
          </div>
          <Show when={failed()}>
            {(error) => <p class="remember-failed">{error()}</p>}
          </Show>
          <div class="remember-actions">
            <button
              type="button"
              class="remember-save"
              disabled={text().trim() === "" || saving()}
              onClick={() => void save()}
            >
              {saving() ? "Filing…" : "Remember"}
            </button>
            <button type="button" class="remember-cancel" onClick={() => props.onClose()}>
              Cancel
            </button>
          </div>
        </div>
      </Show>
    </Overlay>
  );
}
