import { createEffect, createSignal, on, type JSX } from "solid-js";

import { BALLOT_X } from "../glyphs.ts";
import { PENDING_KILL_TIMEOUT_MS } from "../subagents.ts";

/**
 * A stop that asks twice.
 *
 * The terminal stops anything in its dock with one `x` on the row the cursor
 * already sits on (`app/agent_view/panes.rs:502-523`) — two gestures, because
 * the cursor had to get there first. A button in a list is one gesture, and
 * none of the three things this stops is undoable: a subagent's turn is
 * cancelled where it stands, a killed command does not come back, and a deleted
 * schedule is gone. So the first press arms and the second sends.
 *
 * The arming expires on its own after the pager's own `PENDING_KILL_TIMEOUT_SECS`
 * (`app/agent.rs:153`), which the terminal uses for the same idea — how long a
 * stop that has not resolved keeps a row marked — so a click forgotten about
 * does not sit waiting to become a stop. It rides the shared tick rather than a
 * timer of its own, which is what makes the label change without a second
 * click.
 *
 * One component for all three sections, so the discipline cannot be set once
 * and then quietly relaxed in the section added next.
 */
export function StopButton(props: {
  /** The shared wall-clock signal; reading it is what expires the arming. */
  tick: () => number;
  /** A stop was sent and the agent has not answered: the button is spent. */
  pending?: boolean;
  /** The verb, for a row where "stop" is the wrong word. */
  verb?: string;
  /** What the button says while {@link pending} stands. */
  pendingLabel?: string;
  /** What is about to happen, in full, on hover. */
  title: string;
  /**
   * What this button stops, as an id.
   *
   * A rail row is addressed by position, so a finished row leaving the list
   * slides the next one into the button that was standing over it. Naming the
   * subject disarms the button the moment that happens: a click meant for one
   * child must not become a stop for the one that took its place.
   */
  subject?: string;
  onConfirm: () => void;
}): JSX.Element {
  const [armedAt, setArmedAt] = createSignal(0);

  createEffect(on(() => props.subject, () => setArmedAt(0), { defer: true }));

  const armed = (): boolean => {
    const at = armedAt();
    if (at === 0) return false;
    // Read the tick so the arming lapses on its own rather than on the next
    // click; `tick()` is the same signal the running accents ride.
    void props.tick();
    return performance.now() - at < PENDING_KILL_TIMEOUT_MS;
  };

  const press = (): void => {
    if (!armed()) {
      setArmedAt(performance.now());
      return;
    }
    setArmedAt(0);
    props.onConfirm();
  };

  const verb = (): string => props.verb ?? "stop";

  return (
    <button
      class="stop-button"
      classList={{ armed: armed() }}
      type="button"
      disabled={props.pending === true}
      title={props.title}
      onClick={press}
    >
      {props.pending === true
        ? (props.pendingLabel ?? "stopping…")
        : armed()
          ? `confirm ${verb()}`
          : `${BALLOT_X} ${verb()}`}
    </button>
  );
}
