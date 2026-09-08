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
}

export interface SessionUpdateToolCallUpdate {
  sessionUpdate: "tool_call_update";
  toolCallId: string;
  title?: string;
  status?: ToolCallStatus;
  content?: ToolCallContent[];
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
