// The model picker: the terminal's `/model` screen, from data this client is
// already sent.
//
// Nothing here is new on the wire and nothing in Rust changed for it. Where the
// catalog comes from, both paths verified in Rust:
//
//   - `session/new` and `session/load` replies, field `models`
//     (`xai-grok-shell/src/agent/mvp_agent/session_setup.rs:751-754` and
//     `:1107-1110` build the replies with `.models(...)`);
//   - `initialize` reply, `_meta.modelState` — the pre-session catalog, which
//     the pager reads for its session-less dashboard
//     (`xai-grok-pager/src/acp/mod.rs:591-595`). This client has no
//     session-less model screen, so it reads the session replies; `readModelState`
//     accepts both because the same catalog rides both.
//
// `gateway.ts` used to call `session/load` and discard the reply entirely, which
// is why this screen looked like a missing mechanism and was a discarded field.
//
// The switch is standard ACP, not an `x.ai/*` extension: `session/set_model`
// (`agent-client-protocol-schema-0.11.4/src/agent.rs:4096`), implemented by the
// shell at `agent/mvp_agent/acp_agent.rs:2425`. The pager sends the same request
// (`xai-grok-pager/src/app/effects/mod.rs:1868`).

/**
 * ACP `ModelInfo`. camelCase on the wire
 * (`agent-client-protocol-schema-0.11.4/src/agent.rs:3245-3262`).
 *
 * Everything beyond `name`/`description` lives in `_meta`, which the shell
 * stamps in `to_acp_model_info` (`xai-grok-shell/src/agent/config.rs:6197-6280`)
 * and the pager reads back key by key (`pager/src/acp/model_state.rs:74-152`).
 */
export interface ModelInfo {
  modelId: string;
  name: string;
  description?: string | null;
  _meta?: Record<string, unknown> | null;
}

/** ACP `SessionModelState` (`.../agent.rs:3180-3195`). */
export interface SessionModelState {
  currentModelId: string;
  availableModels: ModelInfo[];
  _meta?: Record<string, unknown> | null;
}

/**
 * One selectable reasoning effort.
 *
 * Serialized from the shell's `ReasoningEffortOption`
 * (`xai-grok-sampling-types/src/types.rs:876-882`) into each model's
 * `_meta.reasoningEfforts`.
 */
export interface EffortOption {
  id: string;
  value: string;
  label: string;
  description?: string | null;
  default?: boolean;
}

/**
 * One row of either phase of the picker.
 *
 * `display`, `matchText` and `description` are the pager's `ArgItem`
 * (`xai-grok-pager/src/slash/command.rs:74-83`) because the filter below
 * searches all three, and dropping one would change which rows match.
 *
 * The pager's fourth field, `insert_text`, is not reproduced. It is a composer
 * concept carrying two things at once — which model or level the row means, and
 * whether more input is expected (a trailing space, `slash/commands/model.rs:140-144`,
 * read back by `ends_with(char::is_whitespace)` at `app/modals.rs:23-29`). A
 * list that is not a text field can say both outright, so they are `id` and
 * `chainsToEffort` here.
 */
export interface PickerRow {
  /** The model id, or the effort option id, this row selects. */
  id: string;
  /** What the list shows. */
  display: string;
  /** What the filter searches, in addition to `display` and `description`. */
  matchText: string;
  description: string;
  /** True when this row opens the effort phase rather than applying. */
  chainsToEffort: boolean;
}

const SUPPORTS_EFFORT_KEY = "supportsReasoningEffort";
const EFFORTS_KEY = "reasoningEfforts";
const EFFORT_KEY = "reasoningEffort";

/**
 * `supportsReasoningEffort`, defaulting to **false** when absent.
 *
 * Not a guess: `supports_reasoning_effort_meta`
 * (`xai-grok-sampling-types/src/types.rs:839-845`) is `unwrap_or(false)`. Note
 * this is the opposite default from `firstParty` and `acceptsImages`, which
 * default to true — so the three cannot be read by one helper.
 */
export function supportsEffort(model: ModelInfo): boolean {
  return model._meta?.[SUPPORTS_EFFORT_KEY] === true;
}

/**
 * The built-in effort menu, used when a model advertises support but sends no
 * usable list.
 *
 * Mirrors `legacy_effort_options` (`pager/src/slash/commands/effort_levels.rs:31-41`)
 * over `EFFORT_LEVELS` (`:9-14`), with descriptions from `effort_description`
 * (`:16-26`). `label` is the lowercase level, as `Display` produces it.
 */
export function legacyEffortOptions(): EffortOption[] {
  return [
    { id: "xhigh", value: "xhigh", label: "xhigh", description: "Extended reasoning" },
    { id: "high", value: "high", label: "high", description: "Heavy reasoning" },
    { id: "medium", value: "medium", label: "medium", description: "Balanced reasoning" },
    { id: "low", value: "low", label: "low", description: "Faster, lighter reasoning" },
  ];
}

/**
 * The effort menu for one model.
 *
 * Three-way, exactly as `reasoning_effort_options_for`
 * (`pager/src/acp/model_state.rs:186-196`): no menu at all when the model does
 * not advertise support; the server list when it is a non-empty array of usable
 * entries; the built-in menu otherwise. An absent key and an unusable one
 * collapse to the same fallback — that is stated as a contract at
 * `xai-grok-sampling-types/src/types.rs:1048-1050`, so it is reproduced rather
 * than improved on.
 */
export function effortOptions(model: ModelInfo): EffortOption[] {
  if (!supportsEffort(model)) return [];
  const raw = model._meta?.[EFFORTS_KEY];
  if (!Array.isArray(raw)) return legacyEffortOptions();
  const parsed = raw.filter(
    (entry): entry is EffortOption =>
      typeof entry === "object" &&
      entry !== null &&
      typeof (entry as EffortOption).id === "string" &&
      typeof (entry as EffortOption).value === "string" &&
      typeof (entry as EffortOption).label === "string",
  );
  return parsed.length > 0 ? parsed : legacyEffortOptions();
}

/** The model's own current effort, from `_meta.reasoningEffort`. */
export function currentEffort(model: ModelInfo | undefined): string | null {
  const raw = model?._meta?.[EFFORT_KEY];
  return typeof raw === "string" ? raw : null;
}

/** Look one model up by id. */
export function modelById(state: SessionModelState, id: string): ModelInfo | undefined {
  return state.availableModels.find((m) => m.modelId === id);
}

/**
 * Phase one: one row per model, in catalog order.
 *
 * Mirrors `build_model_items` (`pager/src/slash/commands/model.rs:125-155`).
 * The `(current)` suffix goes on `display` only, so it never affects the filter
 * — the pager does the same by leaving `match_text` as the bare name.
 */
export function modelRows(state: SessionModelState): PickerRow[] {
  return state.availableModels.map((info) => {
    const isCurrent = info.modelId === state.currentModelId;
    return {
      id: info.modelId,
      display: isCurrent ? `${info.name} (current)` : info.name,
      matchText: info.name,
      description: info.description ?? "",
      chainsToEffort: supportsEffort(info),
    };
  });
}

/**
 * Phase two: one row per effort level for a chosen model.
 *
 * Mirrors `build_effort_items` (`model.rs:158-176`) through
 * `build_effort_arg_items` (`effort_levels.rs:52-75`), including the `(active)`
 * suffix, which is applied only when the chosen model is also the session's
 * current model.
 *
 * **Nothing here matches on text the row does not draw.** The pager's
 * `match_text` used to carry a hidden `"a "`/`"b "`/… sort prefix, which made
 * typing `a` in the effort phase select the xhigh row for no visible reason;
 * this client never reproduced it. The prefix is gone as of `3e446fad` —
 * `match_text` is the insert text (`effort_levels.rs`, `build_effort_arg_items`)
 * and the ordering moved to where the rows are actually ordered — so the two
 * clients now agree, and they agree on the rule rather than by accident.
 *
 * **`id` is the option's `value`, not its `id`, and the two genuinely differ.**
 * `ReasoningEffortOption` says so in as many words — "`id`/`label` are
 * presentation and input; `value` is the canonical value sent on the wire"
 * (`xai-grok-sampling-types/src/types.rs:874-883`) — and a server-supplied
 * option may well be `{ id: "deep", value: "xhigh" }`. The terminal only gets
 * away with putting `id` in `insert_text` because it resolves that token back
 * through `resolve_effort_token_for` before sending
 * (`pager/src/acp/model_state.rs:211-228`); a client that skipped the
 * resolution and sent the id would ask for an effort that does not exist.
 * `matchText` stays the id, because the id is what the terminal filters on.
 */
export function effortRows(state: SessionModelState, modelId: string): PickerRow[] {
  const info = modelById(state, modelId);
  if (!info) return [];
  const isCurrentModel = state.currentModelId === modelId;
  const active = currentEffort(info);
  return effortOptions(info).map((option) => ({
    id: option.value,
    display: isCurrentModel && active === option.value ? `${option.label} (active)` : option.label,
    matchText: option.id,
    description: option.description ?? "",
    chainsToEffort: false,
  }));
}

/**
 * The filter the modal actually applies.
 *
 * A case-insensitive substring over `matchText`, `display` and `description`,
 * preserving catalog order — `app/modals.rs:642-652`, verbatim.
 *
 * `nucleo` is deliberately NOT reproduced. `WEB-DEPS.md` rejects every JS fuzzy
 * library on the grounds that a different algorithm yields a different match
 * set, which introduces divergence instead of removing it. That argument holds
 * for the inline slash dropdown, which does rank with `nucleo`
 * (`pager/src/slash/matcher.rs`) — but the modal opened by Ctrl+M and by the
 * palette does not, and reproducing `contains` exactly is both cheaper and
 * strictly more faithful for this screen.
 */
export function filterRows(rows: PickerRow[], query: string): PickerRow[] {
  const q = query.toLowerCase();
  if (q === "") return rows;
  return rows.filter(
    (row) =>
      row.matchText.toLowerCase().includes(q) ||
      row.display.toLowerCase().includes(q) ||
      row.description.toLowerCase().includes(q),
  );
}

/** A `session/set_model` request, ready for `client.request`. */
export interface SetModelRequest {
  sessionId: string;
  modelId: string;
  _meta?: Record<string, string>;
}

/**
 * Build the `session/set_model` params.
 *
 * The effort rides in `_meta.reasoningEffort` on the same request rather than in
 * a second call — `Effect::SwitchModel` builds it that way
 * (`pager/src/app/effects/mod.rs:1855-1873`), keyed by
 * `REASONING_EFFORT_META_KEY` (`xai-grok-sampling-types/src/types.rs:836`).
 */
export function setModelRequest(
  sessionId: string,
  modelId: string,
  effort?: string | null,
): SetModelRequest {
  const req: SetModelRequest = { sessionId, modelId };
  if (effort) req._meta = { [EFFORT_KEY]: effort };
  return req;
}

/**
 * The `x.ai/settings/set` params for the persisted default.
 *
 * **Both halves are required, and this is the trap.** `/model <name>` in the
 * terminal emits two effects: `Effect::PersistSetting { key: "default_model" }`
 * *and* `Effect::SwitchModel` (`pager/src/app/dispatch/settings/setters.rs:1784-1800`).
 * The shell-side setter for that key only persists
 * (`xai-grok-shell/src/util/config/settings_apply.rs:233-240`), so a client that
 * writes the setting alone saves the preference without switching the live
 * session — while the catalog row's own description promises "Changing this also
 * switches the active session"
 * (`sdk/settings/src/generated/catalog.json`, key `default_model`).
 *
 * The row is already `surface: any` with `kind: dynamicEnum { source:
 * activeModelCatalog }`, i.e. the wire already says its choices come from this
 * very catalog. Nothing is missing but the client.
 *
 * `/model <name> <effort>` is the other case and must NOT be persisted: the
 * effort is session-scoped and travels only in the `_meta` above
 * (`pager/src/slash/commands/model.rs:65-71`, dispatch at
 * `pager/src/app/dispatch/router.rs:970-1010`).
 */
export function defaultModelWrite(
  sessionId: string,
  modelId: string,
): { sessionId: string; key: string; value: string } {
  return { sessionId, key: "default_model", value: modelId };
}

/**
 * Read a catalog out of whichever reply carries one.
 *
 * `initialize` puts it under `_meta.modelState`; `session/new` and
 * `session/load` put it in a top-level `models`. Returns `null` rather than an
 * empty catalog when neither is present, so "this agent sent no models" and
 * "this agent has no models" stay distinguishable.
 */
export function readModelState(reply: unknown): SessionModelState | null {
  if (typeof reply !== "object" || reply === null) return null;
  const record = reply as Record<string, unknown>;
  const meta = record._meta as Record<string, unknown> | undefined | null;
  const candidate = record.models ?? meta?.modelState;
  if (typeof candidate !== "object" || candidate === null) return null;
  const state = candidate as Partial<SessionModelState>;
  if (typeof state.currentModelId !== "string" || !Array.isArray(state.availableModels)) {
    return null;
  }
  return state as SessionModelState;
}

/**
 * Fold a `model_changed` broadcast back into the catalog.
 *
 * The switch is not private to whoever made it: the shell announces every one
 * on `x.ai/session_notification` as `SessionUpdate::ModelChanged`
 * (`xai-grok-shell/src/agent/handlers/model_switch.rs:335-356`), and the leader
 * broadcasts it to every client subscribed to the session. So a terminal
 * pressing Ctrl+M, or a second browser tab, moves this one's picker too — which
 * is the whole reason this client reads the update rather than trusting only its
 * own request's reply.
 *
 * The shell's own doc comment says the originating client "skips applying it"
 * because its in-flight response is authoritative and drives its one "Switched
 * to X" scrollback line (`extensions/notification.rs:828-832`). This client has
 * no scrollback line to draw twice, and the fold is idempotent, so it applies
 * the broadcast unconditionally rather than keeping a pending flag whose only
 * job would be to suppress a no-op.
 *
 * Field names are snake_case: `rename_all` on that enum renames the *tag* only,
 * so the variant's own fields stay as Rust wrote them — pinned by the shell's
 * `model_changed_serializes_snake_case_with_optional_effort`.
 *
 * `reasoning_effort` is omitted when the model has none, so an absent key must
 * leave the previous effort alone rather than clearing it.
 */
export function applyModelChanged(
  state: SessionModelState,
  update: Record<string, unknown>,
): SessionModelState {
  const modelId = update["model_id"];
  if (typeof modelId !== "string" || !modelId) return state;
  const effort = update["reasoning_effort"];
  return {
    ...state,
    currentModelId: modelId,
    availableModels: state.availableModels.map((info) =>
      info.modelId === modelId && typeof effort === "string"
        ? { ...info, _meta: { ...(info._meta ?? {}), [EFFORT_KEY]: effort } }
        : info,
    ),
  };
}
