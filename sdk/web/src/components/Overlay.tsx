import { onMount, type JSX } from "solid-js";

import { focusInto, trapFocus } from "../focus.ts";

/**
 * A modal dialog over the page.
 *
 * It claims `aria-modal` and therefore holds focus, which is one decision and
 * not two — see `focus.ts`. The scrim closes on a click that reaches it, so
 * clicking away is an exit; the card stops the click so a click inside is not
 * one. This is the directory picker's own arrangement, extracted the first time
 * a second surface needed it rather than copied.
 */
export function Overlay(props: {
  label: string;
  onClose: () => void;
  children: JSX.Element;
}): JSX.Element {
  let card!: HTMLElement;
  onMount(() => {
    trapFocus(card, () => props.onClose());
    focusInto(card, ".overlay-close");
  });

  return (
    <div class="overlay-scrim" onClick={() => props.onClose()}>
      <section
        class="overlay"
        ref={card}
        role="dialog"
        aria-modal="true"
        aria-label={props.label}
        onClick={(event) => event.stopPropagation()}
      >
        <header class="overlay-header">
          <h2 class="overlay-title">{props.label}</h2>
          <button class="overlay-close" type="button" onClick={() => props.onClose()}>
            Close
          </button>
        </header>
        <div class="overlay-body">{props.children}</div>
      </section>
    </div>
  );
}
