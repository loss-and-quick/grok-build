import { describe, expect, test } from "bun:test";

import {
  applyModelChanged,
  currentEffort,
  defaultModelWrite,
  effortOptions,
  effortRows,
  filterRows,
  legacyEffortOptions,
  modelById,
  modelRows,
  readModelState,
  setModelRequest,
  supportsEffort,
  type SessionModelState,
} from "../src/models.ts";

// Every expectation below is pinned against a named line of the
// pager, because the claim this module makes is not "a model picker works" but
// "the browser can reproduce the terminal's picker from data it already has".
// A test that only checked the TypeScript would prove the first and not the
// second.

/** Shaped like what `session/new` actually replies with. */
const state: SessionModelState = {
  currentModelId: "grok-4.5",
  availableModels: [
    {
      modelId: "grok-4.5",
      name: "grok-4.5",
      description: "Frontier reasoning",
      _meta: { supportsReasoningEffort: true, reasoningEffort: "high", totalContextTokens: 2_000_000 },
    },
    {
      modelId: "grok-4-fast",
      name: "grok-4-fast",
      description: "Lower latency",
      _meta: { supportsReasoningEffort: false },
    },
    { modelId: "bare", name: "bare" },
  ],
};

describe("the catalog is already in replies this client makes", () => {
  test("`session/new` carries it in `models`", () => {
    // `session_setup.rs:751-754` builds the reply with `.models(...)`.
    const reply = { sessionId: "s1", models: state, configOptions: [] };
    expect(readModelState(reply)).toEqual(state);
  });

  test("`initialize` carries it in `_meta.modelState`", () => {
    // The pager reads exactly this key: `pager/src/acp/mod.rs:591-595`.
    expect(readModelState({ _meta: { modelState: state } })).toEqual(state);
  });

  test("a reply with neither is null, not an empty catalog", () => {
    // "sent no models" and "has no models" must stay distinguishable: the
    // second is a real state (a chat session), the first is a stale client.
    expect(readModelState({ sessionId: "s1" })).toBeNull();
    expect(readModelState({ models: { currentModelId: "x" } })).toBeNull();
  });
});

describe("effort support is read the way the shell writes it", () => {
  test("`supportsReasoningEffort` defaults to FALSE when absent", () => {
    // `supports_reasoning_effort_meta` is `unwrap_or(false)`
    // (`xai-grok-sampling-types/src/types.rs:839-845`) — the opposite default
    // from `firstParty` and `acceptsImages`, which are `unwrap_or(true)`
    // (`pager/src/acp/model_state.rs:96`, `:135`). Getting this backwards
    // would offer an effort menu for a model that has none.
    expect(supportsEffort(state.availableModels[2]!)).toBe(false);
    expect(supportsEffort(state.availableModels[1]!)).toBe(false);
    expect(supportsEffort(state.availableModels[0]!)).toBe(true);
  });

  test("an unusable `reasoningEfforts` falls back exactly like an absent one", () => {
    // Stated as a contract at `xai-grok-sampling-types/src/types.rs:1048-1050`.
    const absent = { modelId: "m", name: "m", _meta: { supportsReasoningEffort: true } };
    const empty = { ...absent, _meta: { supportsReasoningEffort: true, reasoningEfforts: [] } };
    const junk = {
      ...absent,
      _meta: { supportsReasoningEffort: true, reasoningEfforts: [{ nope: 1 }] },
    };
    const notAnArray = {
      ...absent,
      _meta: { supportsReasoningEffort: true, reasoningEfforts: "high" },
    };
    for (const model of [absent, empty, junk, notAnArray]) {
      expect(effortOptions(model)).toEqual(legacyEffortOptions());
    }
  });

  test("the built-in menu is xhigh/high/medium/low, in that order", () => {
    // `EFFORT_LEVELS` at `pager/src/slash/commands/effort_levels.rs:9-14`.
    expect(legacyEffortOptions().map((o) => o.id)).toEqual(["xhigh", "high", "medium", "low"]);
  });

  test("a model with no support gets no menu at all", () => {
    // Not an empty fallback menu — no menu. `model_state.rs:192-194`.
    expect(effortOptions(state.availableModels[1]!)).toEqual([]);
    expect(effortRows(state, "grok-4-fast")).toEqual([]);
  });

  test("the server list wins when it is usable", () => {
    const model = {
      modelId: "m",
      name: "m",
      _meta: {
        supportsReasoningEffort: true,
        reasoningEfforts: [{ id: "deep", value: "xhigh", label: "Deep", description: "Slow" }],
      },
    };
    expect(effortOptions(model).map((o) => o.id)).toEqual(["deep"]);
  });
});

describe("the two phases render the rows the pager renders", () => {
  test("phase one marks the current model on `display` only", () => {
    // `build_model_items` (`slash/commands/model.rs:125-155`) leaves
    // `match_text` as the bare name, so the suffix cannot affect the filter.
    const rows = modelRows(state);
    expect(rows.map((r) => r.display)).toEqual(["grok-4.5 (current)", "grok-4-fast", "bare"]);
    expect(rows.map((r) => r.matchText)).toEqual(["grok-4.5", "grok-4-fast", "bare"]);
  });

  test("only a reasoning model chains into phase two", () => {
    // The pager says the same thing with a trailing space on `insert_text`
    // (`model.rs:140-144`), detected at `app/modals.rs:23-29`.
    expect(modelRows(state).map((r) => r.chainsToEffort)).toEqual([true, false, false]);
  });

  test("phase two marks `(active)` only for the session's current model", () => {
    // `build_effort_arg_items` gates the suffix on `mark_active`
    // (`effort_levels.rs:63-65`), which `build_effort_items` sets from
    // `is_current_model` (`model.rs:165`).
    expect(currentEffort(state.availableModels[0])).toBe("high");
    expect(effortRows(state, "grok-4.5").map((r) => r.display)).toEqual([
      "xhigh",
      "high (active)",
      "medium",
      "low",
    ]);

    const other: SessionModelState = { ...state, currentModelId: "bare" };
    expect(effortRows(other, "grok-4.5").map((r) => r.display)).toEqual([
      "xhigh",
      "high",
      "medium",
      "low",
    ]);
  });

  test("the hidden nucleo sort prefix is NOT carried over", () => {
    // The pager puts `"a "`, `"b "`, … in `match_text` to steer nucleo's
    // tiebreak (`effort_levels.rs:66-69`) — but the modal that renders this
    // phase filters with `contains` over `match_text` (`app/modals.rs:642-652`),
    // so in the terminal typing `a` matches xhigh through an invisible key.
    // That is a bug, not a behaviour to reproduce.
    //
    // Queried with the prefix itself, so the assertion is about the key and not
    // about the letter: a bare `a` legitimately matches several descriptions.
    const rows = effortRows(state, "grok-4.5");
    expect(rows.map((r) => r.matchText)).toEqual(["xhigh", "high", "medium", "low"]);
    expect(filterRows(rows, "a ")).toEqual([]);
    expect(filterRows(rows, "b ")).toEqual([]);
  });
});

describe("the filter is the modal's filter, not a fuzzy matcher", () => {
  test("case-insensitive substring over all three fields, order preserved", () => {
    // `app/modals.rs:642-652`, verbatim. `WEB-DEPS.md` rejects JS fuzzy
    // libraries because a different algorithm yields a different match set;
    // here the terminal's own matcher for this screen is `contains`, so the
    // match set is reproducible exactly.
    const rows = modelRows(state);
    expect(filterRows(rows, "FAST").map((r) => r.matchText)).toEqual(["grok-4-fast"]);
    expect(filterRows(rows, "latency").map((r) => r.matchText)).toEqual(["grok-4-fast"]);
    expect(filterRows(rows, "current").map((r) => r.matchText)).toEqual(["grok-4.5"]);
    expect(filterRows(rows, "grok").map((r) => r.matchText)).toEqual(["grok-4.5", "grok-4-fast"]);
    expect(filterRows(rows, "").length).toBe(3);
  });

  test("subsequence matching does not happen", () => {
    // A fuzzy matcher would return `grok-4-fast` for `gkf`. The terminal's
    // modal returns nothing, so this client must too.
    expect(filterRows(modelRows(state), "gkf")).toEqual([]);
  });
});

describe("applying a choice", () => {
  test("a bare model switch sends no effort meta", () => {
    expect(setModelRequest("s1", "grok-4-fast")).toEqual({
      sessionId: "s1",
      modelId: "grok-4-fast",
    });
  });

  test("an effort rides in `_meta` on the same request", () => {
    // `Effect::SwitchModel` builds it this way
    // (`pager/src/app/effects/mod.rs:1855-1873`), keyed by
    // `REASONING_EFFORT_META_KEY` (`sampling-types/src/types.rs:836`).
    expect(setModelRequest("s1", "grok-4.5", "xhigh")).toEqual({
      sessionId: "s1",
      modelId: "grok-4.5",
      _meta: { reasoningEffort: "xhigh" },
    });
  });

  test("persisting the default is a SECOND call, and names the catalog's key", () => {
    // The trap: `/model <name>` emits both `PersistSetting` and `SwitchModel`
    // (`app/dispatch/settings/setters.rs:1784-1800`), while the shell-side
    // setter for `default_model` only persists
    // (`shell/src/util/config/settings_apply.rs:233-240`). A client that sends
    // one of the two diverges from the terminal.
    expect(defaultModelWrite("s1", "grok-4-fast")).toEqual({
      sessionId: "s1",
      key: "default_model",
      value: "grok-4-fast",
    });
  });
});

describe("the catalog row for `default_model` already describes this screen", () => {
  test("it is `surface: any` and draws its choices from the live catalog", async () => {
    // The wire already says a non-terminal client should draw this row and
    // where its choices come from. Read from the generated artifact rather
    // than restated, so a change to the catalog fails here.
    const catalog = (await import("../../settings/src/generated/catalog.json", {
      with: { type: "json" },
    })) as unknown as { default: { rows: Array<Record<string, unknown>> } };
    const row = catalog.default.rows.find((r) => r.key === "default_model");
    expect(row).toBeDefined();
    expect(row!.surface).toBe("any");
    expect(row!.kind).toMatchObject({ type: "dynamicEnum", source: "activeModelCatalog" });
  });
});

describe("what a row carries back", () => {
  test("a model row is keyed by model id, not by the name shown", () => {
    // The picker resolves nothing: it hands `id` straight to `session/set_model`.
    // The terminal cannot, because its rows are text to be typed into a
    // composer — so it puts the *name* in `insert_text` and resolves it again
    // with `resolve_by_name_or_id` (`model.rs:52-53`). Two catalog entries can
    // share a display name; ids cannot.
    expect(modelRows(state).map((r) => r.id)).toEqual(["grok-4.5", "grok-4-fast", "bare"]);
  });

  test("an effort row carries the option's VALUE, not its id", () => {
    // `ReasoningEffortOption` says which is which: "`id`/`label` are
    // presentation and input; `value` is the canonical value sent on the wire"
    // (`xai-grok-sampling-types/src/types.rs:874-883`). The terminal puts the id
    // in the text it inserts and converts it back through
    // `resolve_effort_token_for` (`pager/src/acp/model_state.rs:211-228`); a
    // client that skipped that step and sent `deep` would name a level the
    // agent does not have.
    const model = {
      modelId: "m",
      name: "m",
      _meta: {
        supportsReasoningEffort: true,
        reasoningEfforts: [{ id: "deep", value: "xhigh", label: "Deep" }],
      },
    };
    const catalog: SessionModelState = { currentModelId: "m", availableModels: [model] };
    const rows = effortRows(catalog, "m");
    expect(rows.map((r) => r.id)).toEqual(["xhigh"]);
    // And the id stays the thing the filter searches, because it is what the
    // terminal filters on.
    expect(rows.map((r) => r.matchText)).toEqual(["deep"]);
    expect(setModelRequest("s1", "m", rows[0]!.id)._meta).toEqual({ reasoningEffort: "xhigh" });
  });
});

describe("a switch made somewhere else", () => {
  test("`model_changed` moves the catalog, effort and all", () => {
    // Broadcast to every subscriber of the session
    // (`xai-grok-shell/src/agent/handlers/model_switch.rs:335-356`), so a
    // terminal on the same leader moves the browser's picker. snake_case
    // fields: `rename_all` on that enum renames only the tag.
    const next = applyModelChanged(state, { model_id: "grok-4-fast", reasoning_effort: "low" });
    expect(next.currentModelId).toBe("grok-4-fast");
    expect(currentEffort(modelById(next, "grok-4-fast"))).toBe("low");
    // The old current keeps its own effort: the update speaks about one model.
    expect(currentEffort(modelById(next, "grok-4.5"))).toBe("high");
  });

  test("an omitted effort leaves the model's own alone", () => {
    // `reasoning_effort` is `skip_serializing_if = "Option::is_none"`, so absent
    // means "this model has none to report", not "clear it".
    const next = applyModelChanged(state, { model_id: "grok-4.5" });
    expect(next.currentModelId).toBe("grok-4.5");
    expect(currentEffort(modelById(next, "grok-4.5"))).toBe("high");
  });

  test("a frame with no model id changes nothing", () => {
    expect(applyModelChanged(state, { reasoning_effort: "low" })).toBe(state);
  });
});
