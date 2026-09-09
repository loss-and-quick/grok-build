import { For, Show, createEffect, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

import { decisionsFor, type Decision, type Decisions as Arbiter } from "../decisions.ts";
import { focusInto, trapFocus } from "../focus.ts";
import type { Gateway } from "../gateway.ts";
import { BULLET, CHEVRON, GLYPH_WARNING } from "../glyphs.ts";
import { TICKS_PER_SECOND } from "../animation.ts";
import {
  BLINK_TICKS,
  askToNotify,
  composeTitle,
  notifyPermission,
  raise,
  reducedMotion,
  shouldInterrupt,
} from "../notify.ts";
import { FolderTrustCard } from "./FolderTrustCard.tsx";
import { PermissionCard } from "./PermissionCard.tsx";

/**
 * Every unanswered question, and the one that is in the way.
 *
 * Mounted once, above the session rather than inside it, because the reason the
 * folder-trust card was already drawn here has not changed: the leader routes
 * that request to whichever client opened the session and never replays it, so
 * it can arrive before any session is attached (`gateway.ts`, `PendingFolderTrust`).
 * The permission modal joins it there so that one arbiter hands out one slot —
 * two modals stacked on each other would each claim `aria-modal` and only the
 * top one could keep it.
 */
export function Decisions(props: { gateway: Gateway }): JSX.Element {
  const decisions = decisionsFor(props.gateway);
  useNotifier(props.gateway, decisions);

  return (
    <>
      <Show when={decisions.modal()}>
        {(decision) => (
          <DecisionModal decision={decision()} onPark={() => decisions.park(decision().key)} />
        )}
      </Show>
      <StandingBar gateway={props.gateway} decisions={decisions} />
    </>
  );
}

/**
 * The question, over the page, holding focus.
 *
 * Portalled to `document.body` and paired with `inert` on the layout behind it.
 * That pairing is the point. `aria-modal` is a claim that everything outside is
 * hidden, and this client has already fixed one surface that made the claim and
 * did not keep it (`focus.ts`); the navigator drawer went the other way and made
 * containment a *fact* with `inert` instead of promising it. A blocking question
 * is the one surface that should do both — trap the keyboard, and make the page
 * behind genuinely unreachable — because unlike a picker there is no version of
 * this that is safe to click past.
 *
 * **Escape parks rather than closes**, and that is the whole difference between
 * this dialog and every other one here. There is no dismissal that means
 * nothing: silence parks the session actor with no timeout, so a keypress that
 * merely made the card disappear would hang the turn and say nothing about it.
 * Parking is the pager's own Tab/Space (`key_owner.rs:19-35`): the keyboard goes
 * back to the page, the question stays up in the bar below, and the composer
 * still says the agent is waiting.
 */
function DecisionModal(props: { decision: Decision; onPark: () => void }): JSX.Element {
  let card!: HTMLElement;

  onMount(() => {
    const behind = document.querySelector<HTMLElement>(".layout");
    behind?.setAttribute("inert", "");
    onCleanup(() => behind?.removeAttribute("inert"));
    trapFocus(card, () => props.onPark());
    // The first option, not the park button: the pager opens its card with the
    // cursor on an option (`resolve_initial_cursor`,
    // `appearance/permission_cursor.rs:199-221`), and a dialog that opens on
    // its escape hatch reads as though leaving is what it wants.
    focusInto(card, ".permission-option, .trust-option");
  });

  const label = (): string =>
    props.decision.kind === "permission"
      ? props.decision.pending.title
      : `Trust the files in ${props.decision.pending.workspace}?`;

  return (
    <Portal>
      <div class="decision-scrim">
        <section
          class="decision-modal"
          ref={card}
          role="dialog"
          aria-modal="true"
          aria-label={label()}
        >
          <p class="decision-flag">
            <span class="decision-flag-mark" aria-hidden="true">
              {GLYPH_WARNING}
            </span>
            The agent has stopped and is waiting on you.
          </p>
          <Show
            when={props.decision.kind === "permission" ? props.decision.pending : null}
            fallback={
              <Show when={props.decision.kind === "trust" ? props.decision.pending : null}>
                {(pending) => <FolderTrustCard pending={pending()} />}
              </Show>
            }
          >
            {(pending) => <PermissionCard pending={pending()} />}
          </Show>
          {/* Named "Not now" rather than "Close": it does not answer, and the
              card it belongs to is still open behind it. */}
          <button class="decision-park" type="button" onClick={() => props.onPark()}>
            Not now — keep it below
          </button>
        </section>
      </div>
    </Portal>
  );
}

/**
 * What is still unanswered once the modal is out of the way.
 *
 * Two rows with different jobs. A **parked** question is one this browser was
 * shown and set aside, so it gets a way back in. A question **elsewhere** is one
 * for a session that is not on screen; it is not opened from here, because
 * opening it would mean answering for a session whose transcript the reader
 * cannot see — the roster link is the honest affordance.
 */
function StandingBar(props: { gateway: Gateway; decisions: Arbiter }): JSX.Element {
  const parked = (): Decision[] => props.decisions.here().filter((d) => props.decisions.parked(d.key));
  const elsewhere = (): [string, Decision[]][] => [...props.decisions.elsewhere()];

  return (
    <Show when={parked().length > 0 || elsewhere().length > 0}>
      <div class="decision-bar" role="status">
        <For each={parked()}>
          {(decision) => (
            <button
              class="decision-resume"
              type="button"
              onClick={() => props.decisions.unpark(decision.key)}
            >
              <span class="decision-resume-mark" aria-hidden="true">
                {BULLET}
              </span>
              {decision.kind === "permission"
                ? decision.pending.title
                : `Trust the files in ${decision.pending.workspace}?`}
              <span class="decision-resume-open">Answer</span>
            </button>
          )}
        </For>
        <NotifyOffer />
        <For each={elsewhere()}>
          {([sessionId, waiting]) => (
            <a class="decision-elsewhere" href={`/s/${sessionId}`}>
              <span class="decision-elsewhere-count" aria-hidden="true">
                {waiting.length}
              </span>
              {waiting.length === 1 ? "another session is waiting" : "questions on another session"}
              <span class="decision-elsewhere-go" aria-hidden="true">
                {CHEVRON}
              </span>
            </a>
          )}
        </For>
      </div>
    </Show>
  );
}

/**
 * Keep the tab title, and the interruption, honest about what the agent needs.
 *
 * Mounted from `Decisions` because that is already the one component that knows
 * every unanswered question in the page, and a second place holding the same
 * knowledge is a second place for it to be wrong.
 */
function useNotifier(gateway: Gateway, decisions: Arbiter): void {
  const [focused, setFocused] = createSignal(true);
  const [blinkOn, setBlinkOn] = createSignal(true);
  // Which questions have already been announced. Keyed like the decisions, so a
  // question that is answered and asked again announces again, and one that is
  // merely still open does not announce twice — the pager's own rule, where a
  // batch of permissions is one notification rather than one each
  // (`should_suppress_permission_notification`).
  let announced = new Set<string>();
  let wasBusy = false;

  onMount(() => {
    const sync = (): void => {
      setFocused(document.hasFocus() && !document.hidden);
    };
    sync();
    for (const event of ["focus", "blur", "visibilitychange"]) {
      globalThis.addEventListener(event, sync);
    }
    onCleanup(() => {
      for (const event of ["focus", "blur", "visibilitychange"]) {
        globalThis.removeEventListener(event, sync);
      }
    });

    // The flag blinks on a timer rather than on the animation tick: the tick
    // runs at the pager's 30fps to drive colour curves, and a title does not
    // need — or survive — thirty writes a second. `BLINK_TICKS` keeps the
    // cadence the terminal's, expressed in the artifact's own units.
    //
    // A browser asked to reduce motion gets the flag standing still, exactly as
    // a focused terminal does. That is not a divergence: the terminal has no
    // notion of the setting, and its own answer to "someone is looking at this"
    // is already to stop blinking.
    if (reducedMotion()) return;
    const period = (BLINK_TICKS / TICKS_PER_SECOND) * 1000;
    const timer = setInterval(() => setBlinkOn((on) => !on), period);
    onCleanup(() => clearInterval(timer));
  });

  createEffect(() => {
    const waiting = decisions.all();
    const busy = gateway.status() === "running…";
    const name = gateway.attached()?.entry.title ?? null;

    document.title = composeTitle({
      waiting: waiting.length,
      busy,
      sessionName: name,
      focused: focused(),
      blinkOn: blinkOn(),
    });

    // A question nobody has been told about yet, while nobody is looking.
    const fresh = waiting.filter((decision) => !announced.has(decision.key));
    if (fresh.length > 0 && shouldInterrupt(focused())) {
      const first = fresh[0]!;
      raise(
        `${GLYPH_WARNING} Grok needs you`,
        first.kind === "permission"
          ? first.pending.title
          : `Trust the files in ${first.pending.workspace}?`,
        "grok-decision",
      );
    }
    announced = new Set(waiting.map((decision) => decision.key));

    // The turn ending is the terminal's other default event. Only the edge, and
    // only when the turn was not being watched.
    if (wasBusy && !busy && shouldInterrupt(focused())) {
      raise("Grok finished a turn", name ?? "Your session is idle.", "grok-turn");
    }
    wasBusy = busy;
  });
}

/**
 * The one-line offer to be interrupted, shown only when it can be accepted.
 *
 * It appears beside a question rather than in settings because this is the
 * moment the offer means something: the agent is stopped, and the reader has
 * just found out by coming back to the tab. It disappears for good once the
 * browser has an answer either way — a granted permission needs no button, and
 * a denied one must not be asked again.
 */
function NotifyOffer(): JSX.Element {
  const [permission, setPermission] = createSignal(notifyPermission());
  return (
    <Show when={permission() === "default"}>
      <button
        class="decision-notify"
        type="button"
        onClick={() => void askToNotify().then(setPermission)}
      >
        Tell me even when this tab is in the background
      </button>
    </Show>
  );
}
