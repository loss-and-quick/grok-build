// The navigator when the window is too narrow to hold it beside the session.
//
// There were no media queries in this package at all, and the claim that it had
// been "checked at 700–760px" was not supported by anything in it. What the rail
// changed is that the claim stopped being harmless: a third column at 700px is
// not a layout, it is a way of losing two of them.
//
// Below the breakpoint the navigator becomes a drawer, and the drawer is
// deliberately not a dialog. It is the same list of sessions it is at any other
// width; `aria-modal` would be a promise about focus that has to be paid for
// with a trap (see `focus.ts`), so the page behind is made `inert` instead —
// which is not a promise but the browser actually taking it out of reach.
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { render } from "@solidjs/testing-library";

import { App, Home } from "../src/App.tsx";
import type { SocketLike } from "../src/client.ts";

class DeadSocket implements SocketLike {
  send(): void {}
  close(): void {}
  addEventListener(): void {}
}

const RealWebSocket = globalThis.WebSocket;

beforeEach(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = function () {
    return new DeadSocket();
  };
  localStorage.clear();
});

afterAll(() => {
  (globalThis as { WebSocket: unknown }).WebSocket = RealWebSocket;
});

function open() {
  const history = createMemoryHistory();
  history.set({ value: "/", replace: true });
  const { container } = render(() => (
    <MemoryRouter root={App} history={history}>
      <Route path="/" component={Home} />
    </MemoryRouter>
  ));
  return {
    container,
    button: container.querySelector<HTMLButtonElement>(".drawer-button")!,
    main: container.querySelector<HTMLElement>(".main")!,
    sidebar: container.querySelector<HTMLElement>(".sidebar")!,
  };
}

describe("the navigator as a drawer", () => {
  test("the button exists at every width, and says whether it is open", () => {
    // Rendered always and hidden by the stylesheet above the breakpoint: a
    // control that comes and goes with the window changes the tab order under a
    // keyboard user's hands.
    const { button } = open();
    expect(button).not.toBeNull();
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(button.getAttribute("aria-controls")).toBe("navigator");
    button.click();
    expect(button.getAttribute("aria-expanded")).toBe("true");
  });

  test("what is behind it is inert while it is open, and live again after", () => {
    const { button, main } = open();
    expect(main.hasAttribute("inert")).toBe(false);
    button.click();
    expect(main.hasAttribute("inert")).toBe(true);
    button.click();
    expect(main.hasAttribute("inert")).toBe(false);
  });

  test("it claims no modality, because it holds none", () => {
    // The picker claimed `aria-modal` without a trap once and it was fixed as a
    // bug. Nothing here should be able to make that mistake again by accident.
    const { button, container } = open();
    button.click();
    expect(container.querySelector("[aria-modal]")).toBeNull();
  });

  test("Escape closes it and hands focus back to what opened it", () => {
    const { button, sidebar } = open();
    button.click();
    expect(sidebar.contains(document.activeElement)).toBe(true);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(button);
  });

  test("a click on the scrim is a way out too", () => {
    const { button, container } = open();
    button.click();
    const scrim = container.querySelector<HTMLElement>(".drawer-scrim")!;
    expect(scrim).not.toBeNull();
    scrim.click();
    expect(container.querySelector(".drawer-scrim")).toBeNull();
  });
});
