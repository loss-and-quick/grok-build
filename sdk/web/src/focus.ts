// Keeping a modal's promise.
//
// `aria-modal="true"` is not a decoration, it is a claim: everything outside
// this element is hidden from the reader, so focus must not be able to reach it.
// The directory picker made that claim without keeping it — Tab walked straight
// out into the sidebar behind it, where a screen reader had just been told there
// was nothing — and that was fixed as a bug rather than logged as an audit note
// (`6029fb71`). The rule this file exists to hold is the one that came out of
// it: **declaring `aria-modal` without a trap is worse than not declaring it**,
// because the claim is what a person navigates by.
//
// So the trap lives here, once, and every surface that makes the claim calls it.
// A second copy would be a second chance to make the claim and not keep it.
import { onCleanup } from "solid-js";

/**
 * What Tab may reach inside `card`, in the order Tab reaches it.
 *
 * Deliberately not filtered by visibility. Nothing in a card is hidden while it
 * is mounted, and the checks that would establish invisibility (`offsetParent`,
 * `getClientRects`) need layout — so under a DOM without one they would report
 * *everything* as unreachable and empty the trap.
 */
function focusable(card: HTMLElement): HTMLElement[] {
  return [
    ...card.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]),' +
        ' textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
    ),
  ];
}

/**
 * Hold focus inside `card` until the component unmounts.
 *
 * Call it from `onMount`, with the element that carries `role="dialog"`. It
 * registers its own cleanup: the listener goes, and focus returns to whatever
 * opened the dialog — but only if that element is still in the document, since
 * focusing a detached node moves focus to `<body>`, which is the very thing this
 * is here to avoid.
 *
 * Escape closes, because every dialog in this client closes on Escape and a
 * keyboard user should not have to find out which ones do.
 */
export function trapFocus(card: HTMLElement, onClose: () => void): void {
  // Where focus came from, so it can be given back. A dialog that swallows the
  // focus of the button that opened it leaves a keyboard user at the top of the
  // document with no idea why.
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  const onKey = (event: KeyboardEvent): void => {
    if (event.key === "Escape") {
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const items = focusable(card);
    const first = items[0];
    const last = items[items.length - 1];
    if (!first || !last) {
      event.preventDefault();
      return;
    }
    const active = document.activeElement;
    // Focus that is already outside — a click on the page behind, or a browser
    // that moved it to the address bar and back — is brought in rather than
    // left to wander.
    if (!(active instanceof HTMLElement) || !card.contains(active)) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus();
      return;
    }
    if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  };

  document.addEventListener("keydown", onKey);
  onCleanup(() => {
    document.removeEventListener("keydown", onKey);
    if (opener?.isConnected) opener.focus();
  });
}

/** Move focus into the dialog: the named element if it is there, else the first. */
export function focusInto(card: HTMLElement, preferred?: string): void {
  const first = preferred ? card.querySelector<HTMLElement>(preferred) : null;
  (first ?? focusable(card)[0])?.focus();
}
