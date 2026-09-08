import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";
import { createSignal, onMount, type JSX } from "solid-js";

/**
 * The harness itself, under test.
 *
 * Bun resolves with the `node` condition, and every Solid package aims that
 * condition at a *server* build whose `createEffect` and `onMount` are
 * `function (fn) {}`. `test/setup.ts` redirects all three packages to their
 * browser builds; before it did, components rendered once and stood still, and
 * no test said so — the suite was green while every effect and lifecycle in the
 * package was inert.
 *
 * A test environment that is quietly wrong is worse than one that is loudly
 * broken, so these two assertions stand between them.
 */
function Counter(props: { onMounted: () => void }): JSX.Element {
  const [count, setCount] = createSignal(0);
  onMount(() => props.onMounted());
  return (
    <button class="counter" type="button" onClick={() => setCount(count() + 1)}>
      {count()}
    </button>
  );
}

describe("the test environment is a browser Solid, not a server one", () => {
  test("a signal change reaches the DOM, so reactivity is real", () => {
    const { container } = render(() => <Counter onMounted={() => {}} />);
    const button = container.querySelector<HTMLButtonElement>(".counter")!;
    expect(button.textContent).toBe("0");
    button.click();
    expect(button.textContent).toBe("1");
  });

  test("`onMount` runs, so a component may load something when it appears", () => {
    let mounted = false;
    render(() => <Counter onMounted={() => (mounted = true)} />);
    expect(mounted).toBe(true);
  });
});
