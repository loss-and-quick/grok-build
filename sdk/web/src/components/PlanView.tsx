import { Show, type JSX } from "solid-js";

import { GLYPH_WARNING } from "../glyphs.ts";
import type { Gateway } from "../gateway.ts";
import { Markdown } from "./Markdown.tsx";
import { Overlay } from "./Overlay.tsx";

/**
 * The plan this session has saved.
 *
 * `/view-plan`, as a dialog. The terminal opens the same document in its line
 * viewer and reaches it from a status-bar chip; the browser reaches it from the
 * session header, and the document is markdown here because the pager renders
 * it as markdown there (`LineViewerKind::PlanPreview`, opened with
 * `open_markdown_content`).
 *
 * ## Where the text comes from
 *
 * `x.ai/session/plan`, and that is the whole point of the method existing. A
 * plan is a file the agent owns — `~/.grok/sessions/<cwd>/<id>/plan.md` — and
 * until it crossed, only one form of it ever did: the body attached to the
 * `exit_plan_mode` approval request. A client attached while the plan was being
 * written could show it and a client that attached afterwards could not. The
 * terminal was exempt because it read the file off its own disk, which is
 * exactly the asymmetry a browser cannot have; it now asks the same way this
 * does, so neither client is reading a plan the other cannot see.
 *
 * ## What this cannot do
 *
 * It cannot approve. The approval is a reverse-request, `x.ai/exit_plan_mode`,
 * and answering it means sending an `outcome` this client has no surface for —
 * so the notice below says where the question can be answered instead of
 * offering a button that would not be one.
 */
export function PlanView(props: { gateway: Gateway; onClose: () => void }): JSX.Element {
  const plan = () => props.gateway.plan();
  const content = (): string => plan()?.content ?? "";
  const awaiting = (): boolean => plan()?.awaitingApproval === true;

  return (
    <Overlay label="Plan" onClose={props.onClose}>
      <div class="plan">
        {/* First, because it is the only thing here that is about *now*: the
            agent has stopped and a person has to answer before it goes on.
            A client attaching after the request was sent learns it nowhere else
            — the re-issue on resume is the only other time it is said — which
            is why the snapshot carries the flag at all. */}
        <Show when={awaiting()}>
          <p class="plan-awaiting">
            <span class="plan-awaiting-mark" aria-hidden="true">
              {GLYPH_WARNING}
            </span>
            The agent has stopped and is waiting for this plan to be approved. Answer it in the
            terminal running this session; a browser cannot yet.
          </p>
        </Show>
        <Show
          when={content() !== ""}
          fallback={
            <p class="plan-empty">
              {awaiting()
                ? "The plan file is empty, and the agent is asking to leave plan mode anyway."
                : "Nothing has been written to this session's plan yet."}
            </p>
          }
        >
          <div class="plan-body">
            <Markdown text={content()} />
          </div>
        </Show>
        {/* The one part of a plan a client could not work out for itself: the
            path is `grok_home` joined with the URL-encoded cwd and the session
            id, on the agent's machine rather than on this one. */}
        <Show when={plan()?.path}>
          {(path) => <p class="plan-path">{path()}</p>}
        </Show>
      </div>
    </Overlay>
  );
}
