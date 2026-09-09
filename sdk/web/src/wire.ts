// The slice of the wire this client actually speaks.
//
// Two rules govern this file, both from `docs/WEB-UI.md`.
//
// 1. **Nothing generated is restated here.** Panel types come from
//    `sdk/plugin/src/generated/` and colours from `sdk/theme/src/generated/`;
//    re-typing either would be the drift the generators exist to prevent. What
//    *is* hand-written is the conversational protocol, which has no generated
//    form: ACP is an external crate built on `RawValue`, the orphan rule blocks
//    ts-rs on its types, and `x.ai/*` methods dispatch on a bare string with no
//    schema at all. So: describe only what is called, and grow it on demand.
//
// 2. **No name here carries a client prefix.** Every method below is one the
//    pager also calls. If this client ever needs something the wire cannot say,
//    the answer is a change to the wire, not a `web.`-namespaced back door.
//
// ## Framing
//
// Extension methods are ordinary JSON-RPC methods with a leading underscore:
// `agent-client-protocol-0.10.4/src/lib.rs:224` writes `format!("_{}", method)`
// on send and `:285` strips the same prefix on receive. Params are inline —
// there is no `ext_method` envelope on the wire — and the response body is the
// extension's own payload directly as `result`.

import type { PanelViewModel } from "@grok-build/plugin/generated/PanelViewModel.ts";

/** Prefix the ACP crate puts on every extension method. */
export const EXT_PREFIX = "_";

/** ACP version this client speaks. */
export const PROTOCOL_VERSION = 1;

// ---------------------------------------------------------------------------
// Roster — crates/codegen/xai-grok-shell/src/agent/roster.rs
// ---------------------------------------------------------------------------

export type RosterActivity =
  | "working"
  | "idle"
  | "needs_input"
  | "dormant"
  | "completed"
  | "dead";

export type RosterOrigin = { kind: "local" } | { kind: "remote"; host: string };

/**
 * One dashboard row.
 *
 * `cwd` is the field this whole client is built around: a session owns its
 * root, the leader never rewrites it, and grouping the roster by it is what
 * turns a list of sessions into a list of working directories.
 */
export interface RosterEntry {
  sessionId: string;
  title?: string | null;
  cwd: string;
  isWorktree: boolean;
  modelId?: string | null;
  reasoningEffort?: string | null;
  yolo: boolean;
  activity: RosterActivity;
  lastTurnSummary?: string | null;
  resident: boolean;
  lastChangeUnixMs: number;
  origin: RosterOrigin;
}

export interface RosterListResponse {
  sessions: RosterEntry[];
}

/** Params of the `x.ai/sessions/changed` broadcast. */
export interface RosterChanged {
  upserted?: RosterEntry[];
  removed?: string[];
}

// ---------------------------------------------------------------------------
// Content blocks — agent-client-protocol-schema-0.11.4/src/content.rs
// ---------------------------------------------------------------------------

export interface TextContent {
  type: "text";
  text: string;
}

export type ContentBlock =
  | TextContent
  | { type: "image"; mimeType: string; data: string }
  | { type: "audio"; mimeType: string; data: string }
  | { type: "resource_link"; uri: string; name: string }
  | { type: "resource"; resource: unknown };

export function textOf(content: ContentBlock | undefined): string {
  return content && content.type === "text" ? content.text : "";
}

// ---------------------------------------------------------------------------
// Session updates
//
// Two enums share one tag and one params envelope. The standard ACP
// `SessionUpdate` arrives on `session/update`
// (agent-client-protocol-schema-0.11.4/src/client.rs:79); the grok extension
// enum arrives on `_x.ai/session/update` (and the older
// `_x.ai/session_notification`), tagged the same way
// (crates/codegen/xai-grok-shell/src/extensions/notification.rs:446).
//
// A client can therefore keep one dispatch table over `sessionUpdate` and let
// the carrier method be an implementation detail — which is what
// `is_session_update_ext_method` does in the pager
// (crates/codegen/xai-grok-pager/src/acp/mod.rs:20).
//
// Note the casing seam: the extension enum's `rename_all = "snake_case"`
// applies to the tag only, so its *fields* stay snake_case (`view_model`) while
// standard ACP fields are camelCase (`toolCallId`). Both are reproduced below
// exactly as they appear.
// ---------------------------------------------------------------------------

export type ToolCallStatus = "pending" | "in_progress" | "completed" | "failed";

export interface ToolCallContent {
  type: string;
  content?: ContentBlock;
}

export interface SessionUpdateAgentMessageChunk {
  sessionUpdate: "agent_message_chunk";
  content: ContentBlock;
}

export interface SessionUpdateAgentThoughtChunk {
  sessionUpdate: "agent_thought_chunk";
  content: ContentBlock;
}

export interface SessionUpdateUserMessageChunk {
  sessionUpdate: "user_message_chunk";
  content: ContentBlock;
}

export interface SessionUpdateToolCall {
  sessionUpdate: "tool_call";
  toolCallId: string;
  title: string;
  kind?: string;
  status?: ToolCallStatus;
  content?: ToolCallContent[];
  /**
   * The tool's own arguments, and the source the terminal titles a call from.
   *
   * The pager treats `title` as a fallback and reads the real subject out of
   * here per kind — `command`, `file_path`, `pattern`, `url`
   * (`tool_call_to_block`, `pager/src/acp/tracker.rs`). Untyped because the
   * shape is each tool's own; `toolcall.ts` names the fields it reads.
   */
  rawInput?: unknown;
  /**
   * The tool's typed result. `ToolOutput` is `#[serde(tag = "type")]`
   * (`xai-grok-tools/src/types/output.rs`) and `raw_output` is serialized
   * straight through with no rewriting
   * (`shell/src/session/acp_conversion.rs`), so the variant name and its
   * fields arrive intact.
   *
   * Worth saying plainly, because it is the fix to a defect that looked like a
   * browser problem: a shell call's `ToolOutput::Bash` carries
   * `output_for_prompt`, which the shell already built by stripping ANSI —
   * **the clean text is on the wire**, while `content` carries the raw PTY
   * stream.
   */
  rawOutput?: unknown;
}

export interface SessionUpdateToolCallUpdate {
  sessionUpdate: "tool_call_update";
  toolCallId: string;
  title?: string;
  kind?: string;
  status?: ToolCallStatus;
  content?: ToolCallContent[];
  rawInput?: unknown;
  rawOutput?: unknown;
}

/** The grok extension variant that carries a plugin's panel. */
export interface SessionUpdatePluginPanel {
  sessionUpdate: "plugin_panel";
  /**
   * The publishing plugin. Panel ids are plugin-local, so `(plugin, id)` is the
   * key — the pager's own rule
   * (crates/codegen/xai-grok-shell/src/extensions/notification.rs:570).
   */
  plugin: string;
  view_model: PanelViewModel;
}

export interface SessionUpdatePanelClosed {
  sessionUpdate: "panel_closed";
  plugin: string;
  id: string;
}

// ---------------------------------------------------------------------------
// Subagent lifecycle, on the parent session's own update stream
//
// The three variants below (`crates/codegen/xai-grok-shell/src/extensions/
// notification.rs:654`, `:697`, `:724`) are emitted on the PARENT session id,
// so a client subscribed to the parent sees the whole fan-out without asking
// for anything. `emit_subagent_notification` sends them over
// `x.ai/session_notification` (`agent/subagent/spawn.rs:510`), the carrier this
// client already listens on.
//
// Two of the three are persisted, and therefore replayed on `session/load`
// (`session/acp_session_impl/updates.rs:659`, `:741`). `subagent_progress` is
// never written to JSONL — `updates.rs:638` and `agent/subagent/mod.rs:2049`
// both say so in as many words — so it is live-only. A client attaching
// mid-fan-out therefore learns *which* children exist immediately, and *how
// each one is doing* on that child's next tick.
// ---------------------------------------------------------------------------

/**
 * A child was spawned. Emitted before the child's first prompt is dispatched,
 * so the mapping exists before any of the child's own events arrive.
 *
 * Nested fan-out rides the same variant: a grandchild's spawn is emitted on
 * *its* parent's session id, which a client already subscribed to the child
 * receives — the leader clones the parent's subscriber set onto every child
 * (`leader/server.rs:2315`).
 */
export interface SessionUpdateSubagentSpawned {
  sessionUpdate: "subagent_spawned";
  subagent_id: string;
  parent_session_id: string;
  parent_prompt_id?: string | null;
  child_session_id: string;
  /** `"general-purpose"`, `"explore"`, `"plan"`, or a name found on disk. */
  subagent_type: string;
  /** The model's own one-line summary of the task. Not the task prompt. */
  description: string;
  /** `"new"` or `"resumed"` after bootstrap. */
  effective_context_source?: string | null;
  context_normalized?: boolean;
  capability_mode?: string | null;
  persona?: string | null;
  role?: string | null;
  model?: string | null;
  resumed_from?: string | null;
  workflow_run_id?: string | null;
}

/**
 * A live tick while the child runs.
 *
 * The publisher samples every two seconds and emits only when a counter
 * actually moved, with an eight-second heartbeat so a quiet child still says
 * it is alive (`agent/subagent/mod.rs:2014`, `:2066`). So the gap between two
 * ticks is a fact about the child, not a stall in the client.
 *
 * `duration_ms` is the authoritative elapsed time: the agent measures it from
 * the child's real start, not from when a client happened to see the spawn.
 */
export interface SessionUpdateSubagentProgress {
  sessionUpdate: "subagent_progress";
  subagent_id: string;
  parent_session_id: string;
  child_session_id: string;
  duration_ms: number;
  turn_count: number;
  tool_call_count: number;
  tokens_used: number;
  context_window_tokens: number;
  /** 0–100. */
  context_usage_pct: number;
  tools_used: string[];
  error_count: number;
}

/** The child reached a terminal state. One per child; the pager treats a second as a duplicate. */
export interface SessionUpdateSubagentFinished {
  sessionUpdate: "subagent_finished";
  subagent_id: string;
  child_session_id: string;
  /** `"completed"`, `"failed"` or `"cancelled"`. */
  status: string;
  error?: string | null;
  tool_calls: number;
  turns: number;
  duration_ms: number;
  tokens_used?: number;
  /** The child's final answer, when it completed. */
  output?: string | null;
  will_wake?: boolean;
}

/** Anything else on the same carrier; kept so an unknown tag is data, not a crash. */
export interface SessionUpdateOther {
  sessionUpdate: string;
  [key: string]: unknown;
}

export type SessionUpdate =
  | SessionUpdateAgentMessageChunk
  | SessionUpdateAgentThoughtChunk
  | SessionUpdateUserMessageChunk
  | SessionUpdateToolCall
  | SessionUpdateToolCallUpdate
  | SessionUpdatePluginPanel
  | SessionUpdatePanelClosed
  | SessionUpdateSubagentSpawned
  | SessionUpdateSubagentProgress
  | SessionUpdateSubagentFinished
  | SessionUpdateAvailableCommands
  | SessionUpdateOther;

/** Params of both `session/update` and `_x.ai/session/update`. */
export interface SessionNotification {
  sessionId: string;
  update: SessionUpdate;
  /**
   * Everything about the *delivery* rather than the event.
   *
   * Two keys are read, and both decide whether an update is folded in at all
   * rather than what it draws: `eventId` names the line this notification was
   * (or will be) persisted as, which is what a reconnect can ask to resume
   * after, and `isReplay` says the agent is re-sending history rather than
   * living it. See `resume.ts`.
   */
  _meta?: Record<string, unknown>;
}

// ---------------------------------------------------------------------------
// Requests this client sends
// ---------------------------------------------------------------------------

export interface InitializeRequest {
  protocolVersion: number;
  clientCapabilities: {
    fs: { readTextFile: boolean; writeTextFile: boolean };
    terminal: boolean;
  };
}

/**
 * The part of `initialize`'s response this client reads.
 *
 * `_meta` is where the agent puts everything ACP has no field for, and
 * `currentWorkingDirectory` is the leader's own launch directory
 * (`crates/codegen/xai-grok-shell/src/agent/mvp_agent/acp_agent.rs:577`). It is
 * the natural place to start walking from — not a limit on where a session may
 * go, since `session/new` takes any absolute `cwd`.
 */
export interface InitializeResponse {
  protocolVersion: number;
  /**
   * Every way this agent will accept an `authenticate`, in the agent's own
   * order — and the whole contract. `authenticate` rejects an id that is not on
   * this list (`acp_agent.rs`, whose final match arm answers `invalid_params`),
   * and the pager treats it the same way: `dispatch_choose_auth_method` refuses
   * a method it cannot find here. A client may drive what is listed; it may not
   * invent anything else.
   *
   * `AuthMethod` is a `#[serde(untagged)]` enum whose only inhabited variant is
   * `Agent(AuthMethodAgent)`, so each entry arrives as the bare object below
   * (agent-client-protocol-schema-0.11.4/src/agent.rs:511, :593).
   */
  authMethods?: AuthMethod[];
  _meta?: {
    currentWorkingDirectory?: string;
    /**
     * The method the agent installed for itself, and the one a client should
     * authenticate on rather than re-deriving the precedence — `acp_agent.rs`
     * says so in as many words, because re-deriving it client-side has
     * regressed OIDC refresh before. `null` means the agent found no credential
     * on disk, which is exactly when `session/new` and `session/load` fail with
     * `no auth method id provided` (`agent_ops.rs:4494`).
     */
    defaultAuthMethodId?: string | null;
    /**
     * A sign-in this launch restored, in the shape `authenticate` returns.
     * Present only for a plugin sign-in, whose credential no first-party path
     * can re-derive (`auth_method.rs:344`). Its presence means the agent is
     * already authenticated — and authenticating on the restored method would
     * re-drive the plugin's interactive flow, which is the login it exists to
     * spare the user.
     */
    restoredAuthMeta?: Record<string, unknown> | null;
    /**
     * Pre-session builtins, gated on config alone — `acp_agent.rs:586` calls
     * `slash_commands::builtin_commands`, and nothing tool- or session-derived
     * can be evaluated yet. A seed, not the catalog: skills, workflows and a
     * plugin's own commands need a session and arrive with the first
     * `available_commands_update`.
     */
    availableCommands?: AvailableCommand[];
    /**
     * The machine's own id: a UUID the agent persists at `$GROK_HOME/agent_id`
     * (`xai-grok-telemetry/src/id.rs`).
     *
     * What makes an instance a machine rather than an address. It survives a
     * leader restart, a change of port and the difference between the loopback
     * address and the one the same machine answers on over the network, so two
     * URLs carrying one `agentId` are one instance and a client that treats
     * them as two shows the same roster twice.
     */
    agentId?: string;
    /**
     * The id of this leader *process*, minted fresh on every launch
     * (`id.rs`).
     *
     * A generation, not an identity. A value that differs from the one seen
     * last time means the leader restarted, so session ids remembered from
     * before it may no longer exist — which is the difference between "we
     * reconnected" and "we reconnected to a leader that has forgotten
     * everything".
     */
    agentInstanceId?: string;
    /** The machine's name, and so the default label for an instance. */
    hostname?: string;
    /**
     * The agent binary's version.
     *
     * Worth showing because it can be the reason something is refused: a leader
     * accepts a second `grok` only at a matching version.
     */
    agentVersion?: string;
    [key: string]: unknown;
  };
}

export interface PromptRequest {
  sessionId: string;
  prompt: ContentBlock[];
}

export type StopReason =
  | "end_turn"
  | "max_tokens"
  | "max_turn_requests"
  | "refusal"
  | "cancelled";

export interface PromptResponse {
  stopReason: StopReason;
}

/**
 * `x.ai/plugins/panel_action` — a button press routed back to the plugin.
 *
 * camelCase here, unlike the snake_case `view_model` on the way out: the
 * request struct carries `rename_all = "camelCase"`
 * (crates/codegen/xai-hooks-plugins-types/src/lib.rs:737).
 */
export interface PanelActionRequest {
  sessionId: string;
  plugin: string;
  panelId: string;
  buttonId: string;
  inputs: Record<string, string>;
}

export interface PanelActionResponse {
  /** `false` when the session or the plugin is gone; the panel is then stale. */
  delivered: boolean;
}

// ---------------------------------------------------------------------------
// Filesystem — crates/codegen/xai-grok-shell/src/extensions/fs.rs
//
// `sessionId` is optional and this client never sends it: it is only consulted
// for a *relative* path, which is joined onto that session's cwd. Every path
// here is absolute, so the walk is the same whichever session asks — the point
// established by the two tests behind `docs/WEB-UI.md`'s finding that there is
// no server-side gap under the directory picker.
// ---------------------------------------------------------------------------

/** Params of `x.ai/fs/list`. Everything but `path` has an agent-side default. */
export interface FsListRequest {
  path: string;
  depth?: number;
  limit?: number;
  offset?: number;
  includeHidden?: boolean;
  followSymlinks?: boolean;
  respectGitIgnore?: boolean;
  includeGlobs?: string[];
  excludeGlobs?: string[];
}

/**
 * One entry of a listing.
 *
 * `isSymlink` rides only when true — the Rust writes `e.is_symlink
 * .then_some(true)` behind a `skip_serializing_if` — and a symlinked directory
 * still arrives as `"directory"`, because the walk follows links by default. So
 * the flag is decoration on a kind, not a kind of its own.
 */
export interface FsNode {
  name: string;
  path: string;
  type: "directory" | "file" | (string & {});
  isSymlink?: boolean;
  size?: number;
  modifiedAt?: string;
}

export interface FsListResponse {
  nodes: FsNode[];
  /** The page hit `limit`; there are more entries than were sent. */
  truncated: boolean;
}

/** Params of `x.ai/fs/exists`, the only method that separates missing from empty. */
export interface FsExistsResponse {
  exists: boolean;
}

// ---------------------------------------------------------------------------
// Settings — crates/codegen/xai-grok-shell/src/settings/
// ---------------------------------------------------------------------------

/**
 * Which client a row belongs on.
 *
 * A non-terminal client skips `terminal` rows *because the wire says so*, not
 * because it was handed a list of keys to ignore. This is the mechanism that
 * keeps two clients from diverging: a row with no wire form is a row the TUI
 * also stops drawing, in the same build.
 */
export type SettingSurface = "any" | "terminal";

/** `client` rows carry no value on the wire and are refused on write. */
export type SettingOwner = "client" | "shell" | "shellCached";

export interface SettingRow {
  key: string;
  category: string;
  owner: SettingOwner;
  surface: SettingSurface;
  label: string;
  description?: string;
  keywords?: string[];
  kind: { type: string } & Record<string, unknown>;
  restartRequired?: boolean;
  hiddenInMinimal?: boolean;
}

export interface SettingCategory {
  id: string;
  label: string;
}

export interface SettingLock {
  kind: string;
  reason: string;
  hidesValue: boolean;
  adminManaged: boolean;
}

export interface SettingsListResponse {
  catalog: { version: number; categories: SettingCategory[]; rows: SettingRow[] };
  state: {
    values: Record<string, string | number | boolean>;
    locks: Record<string, SettingLock>;
    configPath: string;
    /**
     * The read-only lock, computed on the leader's machine.
     *
     * It is a `stat` of `config.toml`, which a second client cannot perform, so
     * it rides the response instead of being recomputed — the reason a browser
     * can be honest about a setting it must not offer to change.
     */
    configReadOnly?: "fileMode" | "env";
  };
}

/**
 * Rows a non-terminal client should draw.
 *
 * The filter reads `surface` off the wire; it does not carry a list of keys to
 * skip. That is the whole point of the field: a row that stops having a wire
 * form stops being drawn by the TUI in the same build, rather than silently
 * going missing from one client.
 */
export function visibleSettingRows(rows: readonly SettingRow[]): SettingRow[] {
  return rows.filter((row) => row.surface !== "terminal");
}

/**
 * `x.ai/settings/update` — the remote settings snapshot, pushed rather than
 * asked for.
 *
 * Not a reply and not in the catalog above: the shell builds it from the remote
 * settings it just refreshed and sends it with `forward_fire_and_forget`, which
 * reaches **every** attached client rather than the one that caused the refresh
 * (`mvp_agent/mod.rs:2046-2090`). So a cohort flag flipped on the account
 * arrives here without anyone asking, and a client that ignores the method
 * disagrees with the terminal next to it about what is turned on.
 *
 * Only the field this client acts on is declared. The notification carries
 * twenty-odd others — announcements, campaigns, gates, tips — and restating
 * shapes nothing here reads would be inventing a contract to drift from.
 */
export interface SettingsUpdate {
  /** Whether this account's clients draw the consolidated dock. */
  dock_enabled?: boolean | null;
}

// ---------------------------------------------------------------------------
// Permission prompts
//
// Shared modals: the leader broadcasts the request to every subscriber and the
// first answer wins, so a TUI on the same leader can answer for a browser and
// vice versa. What a client must never do is stay silent — the permission
// round-trip has no timeout on the agent side
// (`crates/codegen/xai-grok-workspace/src/permission/prompter.rs`), so an
// unanswered prompt parks the session actor and the turn never ends.
// ---------------------------------------------------------------------------

export type PermissionOptionKind =
  | "allow_once"
  | "allow_always"
  | "reject_once"
  | "reject_always";

export interface PermissionOption {
  optionId: string;
  name: string;
  kind: PermissionOptionKind;
}

export interface RequestPermissionRequest {
  sessionId: string;
  toolCall: { toolCallId: string; title?: string; kind?: string; rawInput?: unknown };
  options: PermissionOption[];
}

export type RequestPermissionOutcome =
  | { outcome: "selected"; optionId: string }
  | { outcome: "cancelled" };

/** Note the doubled key: the tag and the field are both named `outcome`. */
export interface RequestPermissionResponse {
  outcome: RequestPermissionOutcome;
}

/**
 * The reverse-requests the leader asks **every** attached client at once.
 *
 * `is_interaction_request` (`leader/server.rs`), minus the one this client
 * draws. They are broadcast rather than routed to the session's driver, cached
 * by `toolCallId`, replayed to a client that attaches afterwards, and retracted
 * from the losers by an `interaction_resolved` notification — the whole
 * apparatus of "any client may answer, first answer wins".
 *
 * That is precisely why the ones listed here must be met with **silence**. A
 * `method not found` sent by this client is not a statement about this client:
 * it is *the* answer, and it reaches the agent while the person who can
 * actually answer is still reading the card in their terminal. For
 * `x.ai/exit_plan_mode` the agent reads any error other than an undeliverable
 * one as "the client disconnected mid-approval", abandons the tool call and
 * cancels the turn (`acp_session_impl/tool_calls.rs`) — so an attached browser
 * would break plan-mode approval for the terminal beside it.
 *
 * `session/request_permission` is deliberately absent: this client renders it,
 * so its answer is a real one. `x.ai/folder_trust/request` is absent because it
 * is not broadcast at all — it goes to the session's driver alone, once, and a
 * client that stays silent on it leaves a workspace's configuration off with
 * nobody else to ask.
 */
export const SHARED_INTERACTIONS: ReadonlySet<string> = new Set([
  "x.ai/ask_user_question",
  "x.ai/exit_plan_mode",
  "x.ai/mcp/elicit",
]);

// ---------------------------------------------------------------------------
// Folder trust — crates/codegen/xai-grok-shell/src/agent/mvp_agent/folder_trust_prompt.rs
//
// A session opened in a directory that is not in `trusted_folders.toml`
// resolves *untrusted*, and that project's MCP servers, hooks, plugins, LSP and
// permission rules are silently dropped. `x.ai/folder_trust/request` is the
// agent asking a human about it, and a client that does not answer is a client
// whose sessions quietly lose their project's configuration.
//
// The leader routes this one driver-only rather than broadcasting it, unlike a
// permission modal: it is not a tool call, so it has no `toolCallId` to cache,
// replay or retract by — and a durable security grant is the wrong thing to put
// in front of every attached client (`leader/server.rs`,
// `is_interaction_request`). It therefore lands on the client that opened the
// session, once, and is never replayed.
// ---------------------------------------------------------------------------

/** Params of `x.ai/folder_trust/request`. Every field is always present. */
export interface FolderTrustRequest {
  /** The leader routes on this; a request without it never arrives at all. */
  sessionId: string;
  /** The session's own root. */
  cwd: string;
  /**
   * The canonical workspace key — the scope the grant actually covers, which
   * may be an *ancestor* of `cwd`. Worth showing when the two differ: agreeing
   * trusts more than the directory this session sits in.
   */
  workspace: string;
  /** Why the folder is gated: the repo-local config kinds found in it. */
  configKinds: string[];
}

/**
 * The two answers the wire has.
 *
 * The agent's enum carries `#[serde(other)]`, so *anything* that is not
 * `"trust"` decodes to `Reject` — fail-closed by construction. Which means a
 * misspelled outcome silently declines rather than erroring, and this type
 * exists so no such string can be written here.
 */
export type FolderTrustOutcome = "trust" | "reject";

/**
 * The response body — **bare**, not wrapped.
 *
 * Unlike an `x.ai/*` call this client makes, this one is parsed straight off
 * the raw payload (`serde_json::from_str::<FolderTrustResponse>`), so there is
 * no `ExtMethodResult` envelope around it in either direction.
 */
export interface FolderTrustResponse {
  outcome: FolderTrustOutcome;
}

/**
 * The third outcome, which is not a value.
 *
 * The terminal supports trust, reject and *dismiss*, and the three differ in a
 * way that matters: a reject keeps the agent's per-workspace dedup key, so the
 * question is never asked again for the life of the agent, while a dismissal
 * releases it and the next session in that workspace asks afresh. Both leave
 * the folder untrusted.
 *
 * The pager expresses dismissal by dropping the response channel unanswered. A
 * browser has no channel to drop, so the equivalent is a JSON-RPC *error*
 * reply: the agent reads any transport failure as "not a decision", releases
 * the key and stays gated — the same three-way behaviour, reached the only way
 * a socket allows. Staying silent would work too and is worse: it parks the
 * round-trip for the agent's full thirty-minute timeout.
 *
 * What must never be sent is `{"outcome": "dismiss"}`. That decodes to
 * `Reject`, which is the one outcome dismissal is meant not to be.
 */
export const FOLDER_TRUST_DISMISSED = "folder trust left undecided by the user";

/**
 * `session/new` — the request that fixes a session's root.
 *
 * The response carries much more (models, config options, `_meta`); only the id
 * is described here, per the rule at the top of this file.
 */
export interface NewSessionResponse {
  sessionId: string;
}

// ---------------------------------------------------------------------------
// Slash commands — the catalog the shell advertises
//
// `AvailableCommandsUpdate` is a *standard* ACP `SessionUpdate`
// (agent-client-protocol-schema-0.11.4/src/client.rs:97), so it rides the same
// three carriers as every other update and needs no new dispatch — the point
// made at the top of the session-update section.
//
// The shell fills it in `session/slash_commands.rs::available_commands`, and
// `session/load` asks for one on every attach
// (`agent/mvp_agent/session_setup.rs`, `SessionCommand::AdvertiseCommands`), so
// a client that only listens still gets the catalog the moment it attaches.
// ---------------------------------------------------------------------------

/**
 * One advertised command.
 *
 * `input` is serialized without `skip_serializing_if`, so it arrives as `null`
 * rather than absent when a command takes no argument hint. It carries a hint
 * only — never whether arguments are *allowed*: every ACP command accepts
 * free-form text and the shell parses it (`AcpSlashCommand::from`, which sets
 * `has_args: true` unconditionally).
 */
export interface AvailableCommand {
  name: string;
  description: string;
  input?: { hint: string } | null;
  /** Provenance and skill identity; see `commands.ts`. */
  _meta?: Record<string, unknown> | null;
}

export interface SessionUpdateAvailableCommands {
  sessionUpdate: "available_commands_update";
  availableCommands: AvailableCommand[];
}

// ---------------------------------------------------------------------------
// Authentication — crates/codegen/xai-grok-shell/src/agent/auth_method.rs
//                  crates/codegen/xai-grok-shell/src/agent/mvp_agent/acp_agent.rs
//                  crates/codegen/xai-grok-shell/src/extensions/auth.rs
//
// The interactive burden is entirely the agent's. It runs the OAuth2/OIDC
// exchange, binds the loopback callback listener, opens a browser, polls the
// device endpoint or shells out to an external provider, and it mints,
// enriches and persists the credential into `~/.grok/auth.json` itself
// (`auth/flow.rs`, `run_auth_flow_steps`). A client's whole part is to show a
// URL, sometimes show a code, sometimes hand back a pasted one, and be able to
// cancel. That is why a browser can do this at all: not one of those four is a
// terminal capability.
//
// What this client must never call is on the same wire and deliberately
// unused. `x.ai/auth/getBearerToken` hands a client the live bearer
// (`extensions/auth.rs:47`) and `x.ai/setApiKey` lets a client install one
// (`extensions/auth.rs:71`). Neither name appears anywhere in this package,
// and the reasoning is in the README.
// ---------------------------------------------------------------------------

/**
 * One advertised auth method.
 *
 * `_meta.external_provider` is set when the deployment configured an
 * `auth_provider_command` (`auth_method.rs`, `grok_com_auth_method`); the pager
 * reads exactly that key to start the flow in command mode, so this client does
 * too.
 */
export interface AuthMethod {
  id: string;
  name: string;
  description?: string | null;
  _meta?: Record<string, unknown> | null;
}

/**
 * `authenticate`'s `_meta`.
 *
 * Every field is `#[serde(default)]` on the agent (`mvp_agent/mod.rs:1205`,
 * `AuthRequestMeta`), so only what is meant needs sending.
 *
 * `use_oauth` is deliberately absent. It is the `--oauth` CLI flag, and it
 * *forces* the loopback transport. A browser has no CLI flag, and the transport
 * is the deployment's choice — env, then `[auth] login_device_flow`, then a
 * remote feature flag (`auth/flow.rs`, `should_use_device_flow`). Pinning it
 * from here would overrule that choice on the deployment's behalf.
 */
export interface AuthenticateMeta {
  /**
   * Scopes `x.ai/auth/cancel` to this attempt, so a cancel that arrives late
   * cannot tear down the login that already replaced it
   * (`auth/single_flight.rs`, `cancel_for_client_seq`).
   */
  request_seq: number;
  /**
   * Skip cached credentials without clearing them — what "Sign in" means: the
   * user asked for a login, not for whatever happens to be on disk. Unlike
   * `reauth` it destroys nothing, so abandoning the flow leaves every running
   * session working.
   */
  force_interactive?: boolean;
}

/** `authenticate`'s response body; `_meta` carries the account when there is one. */
export interface AuthenticateResponse {
  _meta?: Record<string, unknown> | null;
}

/**
 * How a login presents itself, decided by the agent and reported by
 * `x.ai/auth/get_url` (`auth/flow.rs`, `AuthUrlMode::as_wire_str`).
 *
 * The distinction is not cosmetic: it decides whether a paste box is a lie.
 * Only `loopback` races a pasted code against the callback listener
 * (`oidc/login.rs`, `race_callback_and_client_ui`). `device` polls the token
 * endpoint and `command` waits on a subprocess, and neither reads `code_rx` at
 * all — a box offered there would swallow whatever was typed into it.
 */
export type AuthUrlMode = "loopback" | "device" | "command";

/**
 * `x.ai/auth/get_url`'s reply.
 *
 * Not wrapped: the handler answers through `to_raw_response`
 * (`extensions/auth.rs:118`), so the payload *is* the result.
 *
 * `auth_url` is `null` when no URL was sent — cached credentials settled it,
 * the flow failed early, or this is a second poll, since the receiver is taken
 * once (`take_url_rx`). `mode` is authoritative; `external_provider` is the
 * back-compat flag older clients read.
 */
export interface AuthUrlResponse {
  auth_url?: string | null;
  external_provider?: boolean;
  mode?: AuthUrlMode | null;
}

// ---------------------------------------------------------------------------
// Subagents — the extension methods
//
// `crates/codegen/xai-grok-shell/src/extensions/task.rs:435` dispatches all
// three. They are ordinary ext methods on the same socket, so a browser reaches
// them exactly as the pager does; nothing about them is terminal-shaped.
//
// The pager calls only `cancel` (`app/effects/mod.rs:1778`). `list_running` and
// `get` exist for clients that were not present when the fan-out started —
// `agent/subagent/mod.rs:2050` names the reconnect case in as many words — and
// a browser tab is that client every time it attaches.
// ---------------------------------------------------------------------------

/**
 * One running child, as `x.ai/subagent/list_running` reports it.
 *
 * camelCase, unlike the snake_case session updates above: the DTOs in
 * `task.rs` carry `rename_all = "camelCase"` while the session-update enum's
 * `rename_all` applies to its tag alone.
 *
 * `startedAtEpochMs` is the field that makes this call worth making. Nothing on
 * the update stream carries a start time — `subagent_spawned` has none — so a
 * client that attached mid-fan-out can only date a child from the moment it
 * saw it. This is the agent's own clock, for children that started before the
 * client existed.
 */
export interface SubagentLiveSnapshot {
  subagentId: string;
  parentSessionId: string;
  childSessionId: string;
  subagentType: string;
  description: string;
  startedAtEpochMs: number;
  durationMs: number;
  turnCount: number;
  toolCallCount: number;
  tokensUsed: number;
  contextWindowTokens: number;
  contextUsagePct: number;
  toolsUsed: string[];
  errorCount: number;
}

/**
 * `x.ai/subagent/list_running`'s reply.
 *
 * Only direct children of the session named in the request, and only live ones:
 * the handler's `From` impl asserts the running status is the only one it can
 * receive (`task.rs:178`). A grandchild is reached by asking its own parent.
 */
export interface ListRunningSubagentsResponse {
  subagents: SubagentLiveSnapshot[];
}

/**
 * What cancelling actually did.
 *
 * Three real answers, and the difference matters to a client: only
 * `cancelled` is followed by a `subagent_finished`. For the other two no
 * further event is coming, so a client that waits for one waits forever —
 * which is why the shell added this tag alongside the older `cancelled` bool
 * (`task.rs:79`).
 *
 * `#[serde(other)]` catches a future `kind`, so an unknown value is not a
 * parse failure; it is the case where this client must fall back to the bool.
 */
export type SubagentCancelOutcome =
  | { kind: "cancelled" }
  | { kind: "already_finished"; status: string }
  | { kind: "not_found" }
  | { kind: string };

/** `x.ai/subagent/cancel`'s reply. `outcome` is absent only from an older shell. */
export interface CancelSubagentResponse {
  subagentId: string;
  /** Legacy flag for older pagers: true only when a live child was stopped. */
  cancelled: boolean;
  outcome?: SubagentCancelOutcome | null;
}

// ---------------------------------------------------------------------------
// The context window — crates/codegen/xai-grok-shell/src/session/context_facts.rs
//
// `/context` used to be a pure function compiled into the pager: which rows
// exist, what they are called, what the unlabelled remainder means, how the
// window divides into a drawable partition, and where the advisory band before
// auto-compaction starts. None of that was reachable by a client that is not
// the pager, so a second client could only reimplement it — and a
// reimplementation of a rounding rule or of an advisory threshold is a
// divergence waiting to happen, not a port.
//
// So the agent resolves it once, on `x.ai/session/info`, and every client
// renders the result. The module says what a client is still left to choose:
// "a client picks glyphs or colors or CSS for these structures and draws
// them". That is the whole of this client's part, and nothing below is
// recomputed here.
//
// Every field is `#[serde(default)]` on the agent, and `contextFacts` is itself
// optional: an agent too old to resolve them omits it. A Rust client answers
// that by calling `ContextFacts::resolve` itself; this one cannot — the
// resolver is the thing that was not reachable — so it says the agent did not
// send them rather than inventing a second derivation.
// ---------------------------------------------------------------------------

/**
 * Which part of the window a row accounts for.
 *
 * A client maps this to a glyph and a colour; the resolver never names one, so
 * the facts stay independent of what draws them. `unknown` is what a kind this
 * build has no name for degrades to — and the wire type says the safe direction
 * is *itemized*, since itemized rows are informational and never enter the
 * partition, so a row that cannot be classified is shown rather than counted
 * twice.
 */
export type ContributorKind =
  | "systemPrompt"
  | "messages"
  | "toolSchemas"
  | "unattributed"
  | "free"
  | "itemized"
  | "unknown";

/** One row of the breakdown. */
export interface Contributor {
  kind: ContributorKind;
  label: string;
  tokens: number;
  /** Count-then-noun detail, e.g. `"12 tools"`. */
  detail?: string | null;
  /** For an injected block, the text `tokens` was measured over. */
  text?: string | null;
}

/**
 * The window split into one hundred units, in draw order.
 *
 * **A hundred, so that one unit reads as one percent**: the wire type says a
 * terminal spends them as cells and a browser as percent, and that both agree
 * with a legend printing percentages of the same window. So this is the one
 * structure here a browser can use verbatim — the bands are already the
 * widths.
 *
 * They are also already clamped. The per-category figures are independent
 * estimates over the same request and routinely sum to more than `used`; the
 * resolver clamps in legend order so the measured bands keep their true width
 * and the unattributed remainder is what gets squeezed, "since the remainder is
 * the one band that has no measurement of its own to defend". Re-normalising
 * these on this side would be a second opinion about that.
 */
export interface BarPartition {
  system: number;
  messages: number;
  tools: number;
  unattributed: number;
  free: number;
}

/**
 * Where the window stands relative to the auto-compact trigger.
 *
 * `thresholdPercent` is the trigger the agent resolved for the active model —
 * remote settings, then the user's TOML, then the environment — and not a
 * client-side default. `approaching` is the advisory band below it, a policy
 * number "with no other expression on the wire, which is why it is resolved
 * here instead of being left for each client to hardcode and then disagree
 * about". Both flags are read, never recomputed from `usagePct`.
 */
export interface AutoCompact {
  thresholdPercent: number;
  thresholdTokens: number;
  remainingTokens: number;
  /** The threshold is reached: compaction runs on the next turn. */
  imminent: boolean;
  /** Close enough to plan around. Never true at the same time as `imminent`. */
  approaching: boolean;
}

/**
 * One completed compaction, read back from the session's own log.
 *
 * `ordinal` is a position rather than a wall clock, because the records come
 * out of a log where "now" says nothing about when they ran. A field the agent
 * did not report is absent, never guessed — including the number of
 * conversation items collapsed, which is not measured at all.
 */
export interface CompactionRecord {
  ordinal: number;
  tokensBefore?: number | null;
  tokensAfter: number;
  elapsedMs?: number | null;
  summaryPreview?: string | null;
}

/**
 * What compaction has done to this session.
 *
 * `reportedCount` and `records.length` are two different things and the wire
 * keeps them apart on purpose: a session resumed without a full replay counts
 * compactions it has no record of, and letting the row count stand in for the
 * total would under-report what compaction did to the conversation.
 */
export interface CompactionFacts {
  reportedCount: number;
  records?: CompactionRecord[];
  recoveredTokens: number;
  /** Records with no before count, so their recovery is missing from the total. */
  recordsWithoutRecovery: number;
  elapsedMs: number;
}

/** The resolved context and compaction picture for one session. */
export interface ContextFacts {
  used: number;
  total: number;
  /**
   * Share of the window in use, at full precision — not the snapshot's own
   * pre-rounded `u8`, which the resolver keeps for the threshold comparison
   * alone so the two can never be mistaken for each other.
   */
  usagePct: number;
  /** The rows that partition the window: the contributors, then the remainder. */
  contributors: Contributor[];
  /**
   * Rows itemizing tokens already counted inside `contributors`. Adding these
   * to the contributors would double-count, which is why they are a second
   * list and not more rows in the first.
   */
  itemized?: Contributor[];
  bar: BarPartition;
  autoCompact: AutoCompact;
  turnCount: number;
  toolCallCount: number;
  compaction: CompactionFacts;
}

/**
 * `x.ai/session/info`'s reply (`agent/handlers/session.rs`, `SessionInfoResponse`
 * flattened over `SessionInfoData`).
 *
 * Only what this client draws is described, per the rule at the top of this
 * file. The handler answers `{}` for a session id it has no resident handle
 * for, so every field here has to be optional in practice.
 *
 * **There is no notification carrier for any of it.** This is request/response
 * only — the pager debounces its own asking — so a client refreshes on attach,
 * at the end of a turn and when a person asks, and never on a timer.
 */
export interface SessionInfoResponse {
  sessionId?: string;
  cwd?: string;
  model?: string | null;
  modelDisplayName?: string | null;
  turns?: number;
  contextFacts?: ContextFacts | null;
  /**
   * What this session had queued at the moment it was asked.
   *
   * The same shape `x.ai/queue/changed` broadcasts, built by the same function
   * (`acp_session_impl/prompt_queue.rs`, `build_queue_wire`), so one parser
   * serves both and an attaching client does not draw an empty queue until the
   * next mutation happens to fire.
   *
   * `null`/absent is an agent that could not answer, and it is **not** an empty
   * queue: a client that read the two the same way would paint away prompts the
   * session is still holding (`session/acp_types.rs`, the round-trip test says
   * so in as many words).
   */
  queue?: QueueChanged | null;
}

// ---------------------------------------------------------------------------
// The shared prompt queue — crates/codegen/xai-prompt-queue/src/types.rs
//
// Two carriers, one shape. `x.ai/queue/changed` is broadcast on every queue
// mutation — including the ones that changed nothing, because "the agent
// re-stated the queue" is the only confirmation any mutation gets — and
// `x.ai/session/info` carries the same struct so an attaching client starts
// from the truth instead of from nothing.
//
// **Every mutation is an ext-notification, not a request.** There is no reply
// to wait on and no error to catch; the confirming broadcast is the whole of
// the feedback, and a mutation the session refuses looks exactly like one it
// accepted except that the queue comes back unchanged. That is why this client
// never draws an optimistic queue: the one thing it could do wrong is show a
// row moving that the session did not move.
// ---------------------------------------------------------------------------

/**
 * One image a queued prompt is carrying (`types.rs`, `QueueImageWire`).
 *
 * A manifest, not a transfer: the encoded bytes stay in the session's own copy
 * of the prompt blocks and never ride the broadcast, which goes to every
 * attached client on every mutation. What it buys is the end of a guess — the
 * row text carries `[Image #N]` placeholders, and this says which of those the
 * session actually holds, so an edit box does not look like it is throwing the
 * pictures away.
 */
export interface QueueImageWire {
  /** The `N` of the row text's `[Image #N]` placeholder, not the list index. */
  displayNumber?: number;
  /** `image/png` and friends, when the session recorded one. */
  mimeType?: string | null;
}

/**
 * One queued prompt (`types.rs`, `QueueEntryWire`).
 *
 * The running turn is never in here: it is drawn from the transcript like any
 * other turn, and the queue is what has not started.
 */
export interface QueueEntryWire {
  id: string;
  /**
   * Bumped on every in-place edit, and sent back on `remove` and `interject`.
   *
   * A mutation naming a stale version is a no-op plus a rebroadcast, which is
   * how two clients editing one row settle without either of them locking it.
   */
  version?: number;
  /** The client that enqueued it; never overwritten by an edit. */
  owner?: string | null;
  /** The client that last edited it. */
  lastEditor?: string | null;
  /** `prompt`, `bash`, `command`, `cron`, `parent_agent_message`, … */
  kind?: string;
  text?: string;
  /** Per-prompt texts when several follow-ups were combined (length ≥ 2). */
  combinedTexts?: string[] | null;
  /** 0-based position among queued, not-yet-running prompts. */
  position?: number;
  /**
   * Whether the session will accept an edit, reorder, remove or send-now here.
   *
   * The bit the mutation handlers themselves gate on
   * (`queue_mutation.rs`, `editable_queue_meta_matches`), put on the wire so a
   * client stops inferring it from {@link kind}. `undefined` is an agent too old
   * to say, and the fallback is the rule that guess used to be — a parent
   * agent's message is protected, everything else is not — which was right only
   * because exactly one origin is protected and its kind is named after it.
   */
  editable?: boolean | null;
  /**
   * The images this row carries, in the order the session holds them.
   *
   * `undefined` from an agent too old to answer, which is not the same as `[]`:
   * an agent that answers at all answers for every row, so an empty list means
   * "this row has none" and absence means nobody asked this agent.
   */
  images?: QueueImageWire[] | null;
}

/**
 * `x.ai/queue/changed`'s params, and `session/info`'s `queue`
 * (`types.rs`, `QueueChanged`).
 *
 * The running fields are carried explicitly because the running row is *not* in
 * {@link entries}; they are what lets a client name the turn a send-now would
 * be interrupting without keeping a mirror of its own.
 */
export interface QueueChanged {
  sessionId?: string;
  entries?: QueueEntryWire[];
  runningPromptId?: string | null;
  runningText?: string | null;
  runningKind?: string | null;
  runningCombinedTexts?: string[] | null;
}

// ---------------------------------------------------------------------------
// The saved plan — crates/codegen/xai-grok-shell/src/session/plan_snapshot.rs
//
// A method of its own rather than a field on `session/info`, and the reason is
// worth keeping here because it decides how this client asks: `session/info` is
// a counter poll, re-asked after every settings writeback and at every turn
// boundary, and a plan body is an unbounded document. Bolting the document onto
// the poll makes every poll pay for it.
//
// It is pulled, never pushed. The one moment the agent genuinely has something
// to say — the `exit_plan_mode` approval — it says by sending that request.
// ---------------------------------------------------------------------------

/**
 * `x.ai/session/plan`'s reply (`plan_snapshot.rs`, `SessionPlanResponse`
 * flattened over `PlanSnapshot`).
 *
 * A session id that names nothing resolves to an empty snapshot rather than an
 * error, and an agent too old to know the method answers `method not found` —
 * which is this client's "no plan available", not something to report.
 */
export interface SessionPlanResponse {
  sessionId?: string;
  /**
   * The plan body, absent when nothing has been written yet.
   *
   * A file holding only whitespace is reported absent too: the agent collapses
   * the two before sending, so every reader tests one thing.
   */
  content?: string | null;
  /**
   * Absolute path of the plan file on the agent's machine.
   *
   * The one part of this a client could not compute for itself — it is
   * `grok_home` joined with the URL-encoded cwd and the session id — and the
   * reason this client can name the file it is showing.
   */
  path?: string | null;
  /**
   * Whether an `exit_plan_mode` approval is parked on this plan.
   *
   * The field a browser needs most. `content: null` together with this set is
   * the approved-but-empty case, which still means the agent has stopped and is
   * waiting on a person.
   */
  awaitingApproval?: boolean;
}

// ---------------------------------------------------------------------------
// Background work — the dock's Tasks and Watchers
//
// Nothing here is new on the wire, and that is the whole point of the section.
// The pager's dock builds its Tasks rows from running non-monitor background
// commands and its Watchers rows from running monitors plus scheduled `/loop`
// tasks (`app/agent_view/panes.rs:338-412`), and every one of those reaches a
// client already:
//
//   - `task_backgrounded` announces a bash command that went to the background,
//     and `monitor_description` is what marks it a monitor rather than a run
//     (`extensions/notification.rs:769-787`);
//   - `task_completed` carries the finished task's whole snapshot (`:641`);
//   - `scheduled_task_created` / `_fired` / `_deleted` are the loop's life
//     (`:789-810`);
//   - all of them are persisted and replayed by `session/load`, which is how
//     the pager's own handler comes to test `_meta.isReplay` on them
//     (`acp_handler/background.rs:92`, `:403-420`).
//
// So this client folds the same stream the terminal folds, and the two methods
// below are the ones the terminal already calls to act on a row.
// ---------------------------------------------------------------------------

/**
 * `TaskKind` (`xai-grok-tools/src/computer/types.rs:146`).
 *
 * The only thing that separates a Tasks row from a Watchers row on the snapshot
 * path. On the notification path the same distinction arrives as the presence
 * of `monitor_description`.
 */
export type TaskKind = "bash" | "monitor" | (string & {});

/**
 * `std::time::SystemTime` as serde writes it.
 *
 * Not a number, and worth declaring rather than probing: `TaskSnapshot` derives
 * plain `Serialize` with no `rename_all` and no timestamp helper, so its
 * `start_time` reaches a client as this two-field struct.
 */
export interface WireSystemTime {
  secs_since_epoch: number;
  nanos_since_epoch: number;
}

/** Epoch milliseconds for a serialized `SystemTime`. */
export function epochMs(time: WireSystemTime | null | undefined): number | undefined {
  if (!time || typeof time.secs_since_epoch !== "number") return undefined;
  return time.secs_since_epoch * 1000 + (time.nanos_since_epoch ?? 0) / 1e6;
}

/**
 * One task as `x.ai/task/list` reports it (`extensions/task.rs:394`).
 *
 * snake_case, unlike the camelCase DTOs beside it in that file: `TaskSnapshot`
 * lives in `xai-grok-tools` and carries no `rename_all` at all
 * (`computer/types.rs:190-248`). Only the fields a dock row is built from are
 * described, per the rule at the top of this file.
 *
 * `start_time` is why the call is worth making. `task_backgrounded` carries no
 * timestamp, so the pager dates a task from the moment its own process saw the
 * notification (`acp_handler/background.rs:157`) — which on a replay is the
 * moment of attaching. This is the agent's clock.
 */
export interface TaskSnapshot {
  task_id: string;
  command: string;
  display_command?: string | null;
  cwd?: string;
  start_time?: WireSystemTime;
  end_time?: WireSystemTime | null;
  completed?: boolean;
  kind?: TaskKind;
  description?: string | null;
  is_backgrounded?: boolean;
  /** The session that owns the task; scoped kills and scoped views read it. */
  owner_session_id?: string | null;
}

/** `x.ai/task/list`'s reply. */
export interface ListTasksResponse {
  tasks?: TaskSnapshot[];
}

/**
 * What killing a background task actually did (`computer/types.rs:292`).
 *
 * The same three-way shape `x.ai/subagent/cancel` has, and it matters for the
 * same reason: only `killed` is followed by a `task_completed`. `already_exited`
 * means the row is about to be finished by an event that has already been sent,
 * and `not_found` means the agent has no such task — a stale row from a replay,
 * which the pager drops outright (`app/dispatch/turn.rs:799-807`).
 */
export type KillOutcome = "killed" | "already_exited" | "not_found" | (string & {});

/** `x.ai/task/kill`'s reply (`extensions/task.rs:50`). */
export interface KillTaskResponse {
  taskId?: string;
  outcome?: KillOutcome;
}

/**
 * Who asked for the kill (`extensions/task.rs:33`).
 *
 * A browser is a client UI, which is the default the field takes when it is
 * omitted; it is sent anyway because the other value means bulk teardown and
 * the difference decides whether the model is told its command was killed
 * (`computer/types.rs:309-315`).
 */
export type TaskKillSource = "clientUi" | "teardown";

/** `x.ai/scheduler/delete`'s reply (`extensions/task.rs:416`). */
export interface DeleteScheduledTaskResponse {
  taskId?: string;
  deleted?: boolean;
}

/** `task_backgrounded` (`extensions/notification.rs:769`). */
export interface SessionUpdateTaskBackgrounded {
  sessionUpdate: "task_backgrounded";
  tool_call_id: string;
  task_id: string;
  command: string;
  cwd: string;
  output_file: string;
  /** Present only for a monitor; its absence is what makes the row a Task. */
  monitor_description?: string | null;
  /** The model's own label for the command, preferred over the command itself. */
  description?: string | null;
}

/** `task_completed` (`extensions/notification.rs:641`). */
export interface SessionUpdateTaskCompleted {
  sessionUpdate: "task_completed";
  task_snapshot: TaskSnapshot;
  will_wake?: boolean;
}

/** `scheduled_task_created` (`extensions/notification.rs:789`). */
export interface SessionUpdateScheduledTaskCreated {
  sessionUpdate: "scheduled_task_created";
  task_id: string;
  prompt: string;
  human_schedule: string;
  next_fire_at?: string | null;
}

/** `scheduled_task_fired` (`extensions/notification.rs:795`). */
export interface SessionUpdateScheduledTaskFired {
  sessionUpdate: "scheduled_task_fired";
  task_id: string;
  prompt: string;
  human_schedule: string;
  next_fire_at?: string | null;
  subagent_id?: string | null;
}

/** `scheduled_task_deleted` (`extensions/notification.rs:804`). */
export interface SessionUpdateScheduledTaskDeleted {
  sessionUpdate: "scheduled_task_deleted";
  task_id: string;
  reason?: string;
}
