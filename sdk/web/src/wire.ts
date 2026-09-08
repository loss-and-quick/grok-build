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
  _meta?: {
    currentWorkingDirectory?: string;
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

/**
 * `session/new` — the request that fixes a session's root.
 *
 * The response carries much more (models, config options, `_meta`); only the id
 * is described here, per the rule at the top of this file.
 */
export interface NewSessionResponse {
  sessionId: string;
}
