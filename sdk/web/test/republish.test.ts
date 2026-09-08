// A plugin repainting its panel must not eat what is being typed into it.
//
// The pager states this as a contract in its own module header: a plugin
// "re-publishes its whole panel on every change (a status tick, a timer)", so
// re-publishing "must *preserve* that state rather than rebuild it"
// (`pager/src/views/plugin_panel.rs:3-7`). `PanelState::merge` keeps the live
// `LineEditor` for every input id that still exists and throws away the
// re-published `value`, with a test of its own named
// `merge_reuses_editor_and_discards_new_value`.
//
// The browser had the opposite behaviour, and it turned out to be one layer
// rather than the two it looked like from the store's call site: writing an
// object at a store path *merges* into the record already there, so the panel
// entry — and with it the `<Panel>` — was never the thing being rebuilt. What
// was rebuilt is the block list, which `<For>` keyed by reference, so every
// block's DOM including the `<input>` was thrown away on each tick. The last
// test below pins the store half, because the fix in the component leans on
// it.
import { describe, expect, test } from "bun:test";
import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

import { Panel } from "../src/components/Panel.tsx";
import { createTranscript, panelKey } from "../src/transcript.ts";
import type { PanelAction } from "../src/panel.ts";
import type { SessionUpdate } from "../src/wire.ts";

function publish(title: string, value: string | null, extra = false): PanelViewModel {
  return {
    id: "p",
    title,
    blocks: [
      { kind: "status", items: [{ label: "tick", value: title, tone: "neutral" }] },
      { kind: "input", id: "code", label: "Code", placeholder: null, value, secret: false },
      ...(extra
        ? [
            {
              kind: "input" as const,
              id: "extra",
              label: "Extra",
              placeholder: null,
              value: "x",
              secret: false,
            },
          ]
        : []),
      { kind: "actions", buttons: [{ id: "go", label: "Go", key: null }] },
    ],
  };
}

/** A panel whose view model can be re-published, as a plugin does. */
function mount(first: PanelViewModel) {
  const [viewModel, republish] = createSignal(first);
  const seen: PanelAction[] = [];
  const { container } = render(() =>
    Panel({
      plugin: "probe",
      get viewModel() {
        return viewModel();
      },
      onAction: (action) => seen.push(action),
    }),
  );
  const field = (n = 0): HTMLInputElement =>
    [...container.querySelectorAll("input")][n] as HTMLInputElement;
  const press = (): void =>
    (container.querySelector(".grok-panel-button") as HTMLButtonElement).click();
  return { container, republish, seen, field, press };
}

describe("a panel that repaints while someone types", () => {
  test("keeps the live field rather than building a new one", () => {
    // The identity of the element is the assertion, not the value on it: a
    // rebuilt input loses the caret and the selection even when the text is
    // put back, and a plugin on a one-second timer would move the caret to the
    // end of the line once a second.
    const { republish, field } = mount(publish("first", null));
    const before = field();
    republish(publish("second", null));
    expect(field()).toBe(before);
  });

  test("and discards the value the re-publish carries for that field", () => {
    // The pager's headline property, in its own words: for an id that still
    // exists the editor survives and the block's `value` is ignored.
    const { republish, field, press, seen } = mount(publish("first", null));
    field().value = "abc";
    republish(publish("second", "server-value"));
    expect(field().value).toBe("abc");
    press();
    expect(seen.at(-1)?.inputs).toEqual({ code: "abc" });
  });

  test("but seeds a genuinely new id from the value it was published with", () => {
    const { republish, field, press, seen } = mount(publish("first", null));
    field().value = "abc";
    republish(publish("second", "server-value", true));
    expect(field(1).value).toBe("x");
    press();
    expect(seen.at(-1)?.inputs).toEqual({ code: "abc", extra: "x" });
  });

  test("a first publish still shows the value the plugin set", () => {
    // The discard is about *re*-publishing. A panel that opens with a value
    // already in it — a remembered account name, a default branch — must show
    // it, or the plugin has no way to fill a field at all.
    const { field } = mount(publish("first", "prefilled"));
    expect(field().value).toBe("prefilled");
  });

  test("the status text still follows the re-publish", () => {
    // Preserving state must not freeze the panel: everything that is not
    // interaction state is the plugin's to redraw.
    const { container, republish } = mount(publish("first", null));
    republish(publish("second", null));
    expect(container.querySelector(".grok-panel-chip-value")?.textContent).toBe("second");
    expect(container.querySelector(".grok-panel-title")?.textContent).toBe("second");
  });
});

describe("a panel whose fields come and go", () => {
  test("stops sending a field the plugin has taken away", () => {
    // `PanelActionParams.inputs` is what the plugin reads to decide what was
    // submitted. A field the panel no longer draws must not still be in there:
    // an OAuth panel that removes its code box after a successful exchange
    // would otherwise keep posting the old code back on every later press.
    const { republish, field, press, seen } = mount(publish("first", null, true));
    field().value = "abc";
    field(1).value = "gone";
    republish(publish("second", null));
    press();
    expect(seen.at(-1)?.inputs).toEqual({ code: "abc" });
  });

  test("and follows an id that moves under a field it is already drawing", () => {
    // Walking by position keeps the element; the registration has to follow the
    // block's id anyway, or the value of one field is delivered under another
    // field's name.
    const renamed: PanelViewModel = {
      id: "p",
      title: "second",
      blocks: [
        { kind: "status", items: [{ label: "tick", value: "second", tone: "neutral" }] },
        { kind: "input", id: "other", label: "Other", placeholder: null, value: "y", secret: false },
        { kind: "actions", buttons: [{ id: "go", label: "Go", key: null }] },
      ],
    };
    const { republish, field, press, seen } = mount(publish("first", null));
    field().value = "abc";
    republish(renamed);
    press();
    expect(seen.at(-1)?.inputs).toEqual({ other: "y" });
  });
});

describe("the panel store", () => {
  test("treats a re-publish as a change to the panel, not a new one", () => {
    // Writing an object at a store path merges into what is there, so the
    // record keeps its identity and the widget above it is not torn down. The
    // component's own preservation only means anything while this holds.
    const t = createTranscript();
    const apply = (title: string): void =>
      t.apply({
        sessionUpdate: "plugin_panel",
        plugin: "one",
        view_model: { id: "p", title, blocks: [] },
      } as SessionUpdate);
    apply("first");
    const before = t.panels[panelKey("one", "p")];
    apply("second");
    expect(t.panels[panelKey("one", "p")]).toBe(before!);
    expect(t.panels[panelKey("one", "p")]?.viewModel.title).toBe("second");
  });
});
