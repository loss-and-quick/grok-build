// The live connection, as reactive state.
//
// Everything the wire teaches is imported, not restated: `client.ts` owns the
// socket and the JSON-RPC framing (including the `_` prefix and the
// inconsistently wrapped extension replies), `wire.ts` owns the shapes. This
// module is only the part that has to be reactive — what is attached, and what
// is waiting on an answer.
//
// **Whether the link is up is deliberately not here.** This module used to
// carry a `connection()` signal, and it could not go backwards: a socket that
// closed left it saying `"connected"`, so a restarted leader or a slept laptop
// produced a page that looked alive and refused every control on it. What can
// answer that question is the thing that watches the socket — `watchLink` in
// `client.ts`, read as `link.phase()` in `App.tsx` — and a second, staler
// answer next to it is worse than none, because the two disagree exactly when
// it matters.
import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";

import {
  XAI_API_KEY,
  drivable,
  interactiveMethods,
  noLoginAvailable,
  selectEagerMethod,
  startMode,
  startupNeedsLogin,
  undrivableMessage,
} from "./auth.ts";
import { GatewayClient, gatewayUrl } from "./client.ts";
import { FUZZY_STATUS, createFileSearch, type FileSearch } from "./filesearch.ts";
import type { PanelAction } from "./panel.ts";
import {
  applyModelChanged,
  defaultModelWrite,
  readModelState,
  setModelRequest,
  type SessionModelState,
} from "./models.ts";
import { readIdentity, relaunched, type AgentIdentity } from "./instances.ts";
import {
  createResumption,
  readUpdateMeta,
  streamOf,
  type Arrival,
  type Resumption,
} from "./resume.ts";
import { permissionModeParams, readModeUpdate, type PermissionMode } from "./modes.ts";
import { createRoster, type Roster } from "./roster.ts";
import { createSubagents, type Subagents } from "./subagents.ts";
import { createTasks, type Tasks } from "./tasks.ts";
import { createTranscript, type Transcript } from "./transcript.ts";
import { LIST_PARAMS, ROOT } from "./directory.ts";
import {
  type ContentBlock,
  PROTOCOL_VERSION,
  visibleSettingRows,
  FOLDER_TRUST_DISMISSED,
  type AuthMethod,
  type AuthUrlMode,
  type AuthUrlResponse,
  type AuthenticateResponse,
  type AvailableCommand,
  type CancelSubagentResponse,
  type FolderTrustOutcome,
  type FolderTrustRequest,
  type FolderTrustResponse,
  type FsExistsResponse,
  type FsListResponse,
  type InitializeResponse,
  type DeleteScheduledTaskResponse,
  type KillTaskResponse,
  type ListRunningSubagentsResponse,
  type ListTasksResponse,
  type NewSessionResponse,
  type PanelActionResponse,
  type PromptResponse,
  type RequestPermissionRequest,
  type RequestPermissionResponse,
  type RosterChanged,
  type RosterEntry,
  type RosterListResponse,
  type SessionInfoResponse,
  type SessionNotification,
  type SessionUpdate,
  type SettingRow,
  type SettingsListResponse,
  type SettingsUpdate,
} from "./wire.ts";

/** A shared permission modal waiting for this client to answer it. */
export interface PendingPermission {
  toolCallId: string;
  title: string;
  request: RequestPermissionRequest;
  answer: (response: RequestPermissionResponse) => void;
}

/**
 * A folder-trust card waiting for an answer.
 *
 * Not keyed to the attached session, and not stored on it. The leader routes
 * this request to whichever client opened the session and never replays it, so
 * it can arrive before that session is attached — or while another one is on
 * screen. Discarding it in either case would leave the project's MCP servers,
 * hooks, plugins and LSP silently off, which is the failure the card exists to
 * prevent.
 */
export interface PendingFolderTrust {
  sessionId: string;
  cwd: string;
  workspace: string;
  configKinds: string[];
  /** Answer it: `"trust"` grants, `"reject"` declines for the agent's lifetime. */
  decide: (outcome: FolderTrustOutcome) => void;
  /** Leave it undecided, so the next session in this workspace asks again. */
  dismiss: () => void;
}

/**
 * Where this connection stands with the agent's authentication.
 *
 * `"settled"` is the only state in which `session/new` and `session/load` can
 * succeed: both go through `spawn_and_register_session`, which refuses with
 * `auth_required` — "no auth method id provided" — while the agent has no
 * method installed (`agent_ops.rs:4494`). The whole point of this state is that
 * a browser can reach `"settled"` on its own instead of waiting for a terminal
 * to reach it first.
 */
export type AuthStatus = "unknown" | "settled" | "needed" | "running";

export interface AuthState {
  status: AuthStatus;
  /** Advertised methods, in the agent's order. The order is the contract. */
  methods: AuthMethod[];
  /** The method a login is currently being driven on. */
  driving: string | null;
  /** Scopes a cancel to one attempt; also discards a stale attempt's results. */
  requestSeq: number;
  /** The authorize URL, once the agent has one to give. */
  url: string | null;
  /** How the agent is presenting this login; `null` until `get_url` answers. */
  mode: AuthUrlMode | null;
  /** The last failure, in the agent's own words. */
  error: string | null;
  /**
   * Set when no login can be started from here at all, with the reason and the
   * place it can be done instead. Distinct from `error`: an error invites a
   * retry, and this does not.
   */
  blocked: string | null;
}

/**
 * How long to keep asking for the authorize URL.
 *
 * The pager's own cadence (`pager/src/app/effects/mod.rs:2276`): the receiver
 * is taken once, so the first poll to arrive after the attempt registers blocks
 * until the URL is ready and every later one returns nulls immediately. The
 * retries exist for the poll that arrives *before* the attempt exists.
 */
const AUTH_URL_POLLS = 60;
const AUTH_URL_POLL_GAP_MS = 50;

export interface Attached {
  entry: RosterEntry;
  transcript: Transcript;
  /**
   * The children this session has spawned.
   *
   * Per attach, like the transcript: a fan-out belongs to the session that
   * launched it, and `session/load` replays that session's own
   * `subagent_spawned` and `subagent_finished` rows, so the list rebuilds
   * itself on every attach rather than being carried between sessions.
   */
  subagents: Subagents;
  /**
   * This session's background work: the dock's Tasks and Watchers.
   *
   * Per attach, like the transcript and the fan-out, and rebuilt the same way:
   * `session/load` replays this session's own `task_backgrounded`,
   * `task_completed` and `scheduled_task_*` rows, so the two sections come back
   * from the log rather than being carried between sessions.
   */
  tasks: Tasks;
  /**
   * How much of this session's event log this client has drawn.
   *
   * Per attach like the two above, and — unlike them — it is the one piece that
   * a reconnect deliberately carries *over*: it is the whole reason the next
   * `session/load` can ask for a tail instead of a transcript.
   */
  resumption: Resumption;
}

/**
 * What a socket that has just died was showing.
 *
 * Held across exactly one `connect`, and consumed by the first {@link Attached}
 * that follows it. It exists because `connect` has to drop the attached session
 * — a transcript built from one leader's replay must never sit under another
 * leader's roster — while a *reconnect* wants the opposite: the same transcript
 * back, with only what was missed appended to it. The identity is what keeps
 * those two apart, since a session id is unique on a leader and not between
 * leaders.
 */
interface Carried {
  sessionId: string;
  attached: Attached;
  /** The machine the transcript was built from. */
  agentId: string;
}

/**
 * The two things a reconnect can have to say, in the conversation itself.
 *
 * The status line is not where these go. It is overwritten by the next thing
 * that happens, and both of these are statements about a *point in time* in the
 * session — which is what the transcript is for, and it is also the one place a
 * person scrolling back a minute later will still find them.
 *
 * Nothing is said about an ordinary reconnect. A drop that lost nothing is not
 * an event in the conversation, and a banner on every one of them would train
 * the reader to skip the two that matter.
 */
const RESTARTED_NOTICE =
  "The agent restarted while this page was disconnected. Anything it was running at the time did not survive, and prompts that were waiting to run may not have either.";
const REBUILT_NOTICE =
  "This conversation was reloaded from the agent, because it could no longer say what had changed since this page last saw it. What is above is the agent's own copy.";

/**
 * The status line for a finished attach.
 *
 * The count is not decoration. "Only what was missed" is the claim this whole
 * mechanism makes, and a number beside it is the one thing that makes the claim
 * checkable by the person it was made to — a resume of a long conversation that
 * reports six updates is doing what it says, and one that reports six hundred
 * has fallen back and says so on the next line as well.
 */
function describeArrival(sessionId: string, resumed: boolean, arrived: Arrival): string {
  if (!resumed) return `attached to ${sessionId}`;
  if (arrived.rebuilt) return `resumed ${sessionId}; the conversation was reloaded`;
  if (arrived.frames === 0) return `resumed ${sessionId}; nothing was missed`;
  const plural = arrived.frames === 1 ? "" : "s";
  return `resumed ${sessionId}; caught up on ${arrived.frames} update${plural}`;
}

const STORE_KEYS = {
  url: "grok-gateway",
  secret: "grok-secret",
  theme: "grok-theme",
  /** This browser's answer about the widget rail; `""` means it has not said. */
  rail: "grok-rail",
} as const;

export function remembered(key: keyof typeof STORE_KEYS, fallback = ""): string {
  try {
    return localStorage.getItem(STORE_KEYS[key]) ?? fallback;
  } catch {
    return fallback;
  }
}

export function remember(key: keyof typeof STORE_KEYS, value: string): void {
  try {
    localStorage.setItem(STORE_KEYS[key], value);
  } catch {
    // A page opened with site data blocked still works; it just forgets.
  }
}

export function createGateway() {
  const [status, setStatus] = createSignal("not connected");
  const [attached, setAttached] = createSignal<Attached | null>(null);
  // Where the directory picker starts walking. The agent names its own launch
  // directory in `initialize`'s `_meta`; the root is the fallback because it is
  // the one path that always exists, and it is never a *limit* — `session/new`
  // takes any absolute `cwd`, so the picker may leave in either direction.
  const [agentCwd, setAgentCwd] = createSignal(ROOT);
  // Who answered. `initialize._meta` has carried this since long before there
  // was anything here to read it: the machine's persistent id, the leader
  // process's own id, the hostname and the agent's version. Two of the five
  // keys were read and the rest dropped, which is why this client could name
  // the address it was talking to and not the machine.
  const [identity, setIdentity] = createSignal<AgentIdentity>({});
  // Whether the socket that has just come up reaches a *different process* on
  // the machine the last one reached. False on a first connect and on a switch
  // to another machine: neither of those is a leader that restarted under a
  // page that was watching it. Recomputed on every `connect`, so it describes
  // the current socket and not the history of the tab.
  const [relaunch, setRelaunch] = createSignal(false);
  // Whether the agent says this account draws the dock. `null` until the wire
  // says something: the notification arrives when remote settings are refreshed,
  // which may not happen at all while this page is up, and "not told" is not
  // "told no". Reset on every connect, because it is the agent's answer and the
  // next agent may give a different one.
  const [dockEnabled, setDockEnabled] = createSignal<boolean | null>(null);
  // The attached session's `x.ai/session/info`, which is where the resolved
  // context window arrives. `null` means nobody has asked yet — not that the
  // window is empty — and it is dropped on every attach, because the facts
  // belong to one session and a stale breakdown under a new title is a wrong
  // answer where "not yet" is the true one.
  const [sessionInfo, setSessionInfo] = createSignal<SessionInfoResponse | null>(null);
  // The slash catalog for the attached session.
  //
  // Two sources, and the seam between them is the point. `initialize` carries
  // the shell's pre-session builtins, which is all it *can* carry: skills,
  // workflows and a plugin's own commands are resolved per session, against
  // that session's cwd and tool set. The real catalog arrives as
  // `available_commands_update` — and `session/load` asks for one on every
  // attach (`SessionCommand::AdvertiseCommands`), so a client that merely
  // listens is served, and served again whenever the model, the plugins or the
  // skills on disk change.
  //
  // Per session, therefore, and reset on every attach: holding the previous
  // session's list would offer a plugin's command in a directory whose plugin
  // is not installed.
  const [seedCommands, setSeedCommands] = createSignal<AvailableCommand[]>([]);
  const [commands, setCommands] = createSignal<AvailableCommand[]>([]);
  // The model catalog for the attached session, straight off the `session/load`
  // reply. `null` means no catalog has been sent, which is not the same as an
  // empty one — see `readModelState`.
  const [models, setModels] = createSignal<SessionModelState | null>(null);
  const [settings, setSettings] = createStore<{ rows: SettingRow[]; terminalOnly: number; values: Record<string, unknown>; locks: Record<string, { reason: string }> }>({
    rows: [],
    terminalOnly: 0,
    values: {},
    locks: {},
  });
  const [auth, setAuth] = createStore<AuthState>({
    status: "unknown",
    methods: [],
    driving: null,
    requestSeq: 0,
    url: null,
    mode: null,
    error: null,
    blocked: null,
  });
  // The mode the agent has *confirmed*, never the one that was asked for. Null
  // until a `current_mode_update` arrives, because until then nothing is known.
  const [sessionMode, setSessionMode] = createSignal<string | null>(null);
  const [permissions, setPermissions] = createStore<PendingPermission[]>([]);
  const [folderTrusts, setFolderTrusts] = createStore<PendingFolderTrust[]>([]);
  const roster: Roster = createRoster();

  let client: GatewayClient | null = null;
  /**
   * The socket and session the last {@link attach} was made on.
   *
   * `session/load` is not idempotent from the client's side: it makes the agent
   * replay the whole transcript **to the asking client** (`replay_session_updates`,
   * `xai-grok-shell/src/agent/mvp_agent/replay.rs:190`, routed to one client by
   * `_meta["x.ai/leaderClientId"]` at `leader/server.rs:2135-2176`). So a second
   * load on one socket lands a second copy of every frame, and because the
   * replays interleave with the *later* attach's fresh transcript, the result is
   * every turn twice — the user's included.
   *
   * It is compared by socket identity, not by session id alone, because
   * re-attaching after a reconnect is *required*: ACP v1 replays nothing to a
   * client that was not there, so the new socket has to ask again. The pair says
   * exactly what the leader's own books say — a subscription is `(client,
   * session)` (`leader/server.rs:2000-2012`) — and a `connect` builds a new
   * client, so the pair goes stale on its own without being reset anywhere.
   */
  let attachedOn: { client: GatewayClient; sessionId: string } | null = null;
  /** See {@link Carried}. Written by `connect`, read once by `attach`. */
  let carried: Carried | null = null;

  /**
   * Take the carry-over, if it is this session on this machine.
   *
   * Consumed either way, and that is deliberate: it describes what the socket
   * that just closed was showing, and one attach later it describes nothing.
   * Leaving it would let a switch to another session and back reuse a
   * transcript whose cursor has been overtaken by everything that happened in
   * between.
   *
   * The machine check is `agentId`, not `agentInstanceId`: a leader that has
   * restarted is still the same machine with the same session files, so a
   * cursor written against them still resolves. What a restart invalidates is
   * the running state, not the log — which is why it changes what the person is
   * told (see {@link RESTARTED_NOTICE}) and not whether the cursor is sent.
   */
  const takeCarried = (sessionId: string): Attached | null => {
    const held = carried;
    carried = null;
    if (!held || held.sessionId !== sessionId) return null;
    return held.agentId === identity().agentId ? held.attached : null;
  };

  const say = (text: string): void => {
    setStatus(text);
  };

  /**
   * `@`-completion, rooted in the attached session's cwd.
   *
   * Its lifetime is argued where it lives ({@link createFileSearch}). What
   * belongs here is only the plumbing it cannot reach: a socket that may not
   * exist, and the session whose id the leader routes the status stream by.
   */
  const fileSearch: FileSearch = createFileSearch({
    ext: async (method, params) => {
      if (!client) throw new Error("gateway is not connected");
      return client.ext(method, params);
    },
    session: () => {
      const current = attached();
      return current ? { sessionId: current.entry.sessionId, cwd: current.entry.cwd } : null;
    },
    say,
  });

  /**
   * Answer `session/request_permission`.
   *
   * Options are rendered from the `options` array as sent, never a hardcoded id
   * list: which options exist depends on the tool and on the client type the
   * leader registered. The modal is shared — another attached client can answer
   * first, in which case `interaction_resolved` arrives and takes this card
   * down, and the promise settles `cancelled`, which the agent discards because
   * it already has its answer.
   */
  const askPermission = (request: RequestPermissionRequest): Promise<RequestPermissionResponse> =>
    new Promise((resolve) => {
      const toolCallId = request.toolCall?.toolCallId ?? "";
      const settle = (response: RequestPermissionResponse): void => {
        setPermissions((all) => all.filter((p) => p.toolCallId !== toolCallId));
        resolve(response);
      };
      setPermissions(permissions.length, {
        toolCallId,
        title: request.toolCall?.title ?? "Permission requested",
        request,
        answer: settle,
      });
    });

  /**
   * Answer `x.ai/folder_trust/request`.
   *
   * Three outcomes, and they are genuinely three. `"trust"` persists the grant
   * and hot-reloads that workspace's MCP servers, hooks and plugins for every
   * resident session on it. `"reject"` leaves it gated and keeps the agent's
   * per-workspace dedup key, so it is never asked again for the agent's
   * lifetime. Dismissal rejects this promise instead, which reaches the agent
   * as a JSON-RPC error: it reads that as "not a decision", releases the key
   * and asks the next session in that workspace afresh.
   *
   * What none of the three may be is *silence*. The agent waits half an hour on
   * this round-trip, and the session sits with its project configuration off
   * for all of it.
   */
  const askFolderTrust = (request: FolderTrustRequest): Promise<FolderTrustResponse> =>
    new Promise((resolve, reject) => {
      const sessionId = request.sessionId ?? "";
      const close = (): void => {
        setFolderTrusts((all) => all.filter((t) => t.sessionId !== sessionId));
      };
      setFolderTrusts(folderTrusts.length, {
        sessionId,
        cwd: request.cwd ?? "",
        workspace: request.workspace ?? "",
        configKinds: request.configKinds ?? [],
        decide: (outcome) => {
          close();
          say(
            outcome === "trust"
              ? `trusted ${request.workspace}`
              : `left ${request.workspace} untrusted`,
          );
          resolve({ outcome });
        },
        dismiss: () => {
          close();
          say(`${request.workspace} left undecided; it will be asked again`);
          reject(new Error(FOLDER_TRUST_DISMISSED));
        },
      });
    });

  const onNotification = (method: string, params: unknown): void => {
    if (method === "x.ai/sessions/changed") {
      roster.apply((params ?? {}) as RosterChanged);
      return;
    }
    // The remote settings snapshot, broadcast to every client. Dropping it is
    // how this page came to disagree with the terminal beside it about whether
    // the dock is on for this account. Only `dock_enabled` is read; a field
    // that is absent or null is the agent saying nothing about it, which is not
    // the same as saying no.
    if (method === "x.ai/settings/update") {
      const flag = (params as SettingsUpdate | undefined)?.dock_enabled;
      if (typeof flag === "boolean") setDockEnabled(flag);
      return;
    }
    // One batch of `@`-completion results. The leader routes it by the session
    // id in its own params, so a terminal searching in the same session lands
    // here too; the search discards any batch that is not under its own id.
    if (method === FUZZY_STATUS) {
      fileSearch.apply(params);
      return;
    }
    // One dispatch for all three carriers: standard ACP `session/update`, the
    // grok extension's live `x.ai/session_notification`, and the replay-time
    // `x.ai/session/update`. Same envelope, same `sessionUpdate` tag, so the
    // carrier is not a fork in the client.
    if (
      method === "session/update" ||
      method === "x.ai/session/update" ||
      method === "x.ai/session_notification"
    ) {
      const notification = params as SessionNotification | undefined;
      const current = attached();
      if (!notification?.update || !current) return;
      const update = notification.update;
      // A subagent's own session is a session this client is subscribed to
      // without ever having asked: the leader copies the parent's subscriber
      // set onto each child at spawn (`leader/server.rs:2315`). Its frames
      // arrive with the *child's* session id, so a client that only recognises
      // the attached one throws away the entire fan-out. They are not the
      // attached session's transcript, though — they feed the child's row.
      if (notification.sessionId !== current.entry.sessionId) {
        if (!current.subagents.childSessions().has(notification.sessionId)) return;
        // A grandchild's spawn is announced on its own parent's id, so the
        // lifecycle fold sees it here rather than on the attached session.
        current.subagents.apply(notification.sessionId, update);
        current.subagents.applyChild(notification.sessionId, update);
        return;
      }
      // Every frame for the attached session goes through the gate, not only
      // the ones the transcript draws. The cursor names a position in one file
      // that all of them are written to, so skipping a tag here would leave the
      // cursor behind the log — and the agent refuses a cursor whose tail
      // contains a line it cannot send as live, which turns "behind" into a
      // full replay (`session/storage/replay.rs:508-518`).
      const tag = update.sessionUpdate;
      const meta = readUpdateMeta(notification._meta);
      const verdict = current.resumption.verdict(streamOf(method), tag, meta);
      // Already drawn. The leader drops most of these itself — it holds live
      // notifications back while a load is in flight and discards the ones the
      // replay had already covered (`leader/server.rs:2062-2078`) — and this is
      // the half that does not depend on which leader answered.
      if (verdict === "duplicate") return;
      // The cursor did not resolve and the whole transcript is arriving behind
      // this frame. Everything on screen is about to be re-sent, so it goes
      // now, before the first line of the new copy is folded in.
      if (verdict === "rebuild") current.transcript.reset();
      // Ahead of the transcript fold rather than inside it, because this is the
      // one fold that needs to know *how* the frame arrived: a live
      // `task_backgrounded` means the command started as it was announced, and
      // a replayed one says nothing at all about when.
      current.tasks.apply(update, meta.isReplay);
      fold(current, notification.sessionId, update);
      // After the fold, never instead of it: what the cursor is allowed to name
      // depends on how many entries the transcript holds at that moment.
      current.resumption.drew(tag, meta);
    }
  };

  /**
   * Put one update where it belongs.
   *
   * Split out of the dispatch above only so that every path through it is
   * followed by the same bookkeeping. Three tags are not transcript at all —
   * the slash catalog, the model, and a question another client answered — and
   * each used to return early, which would now mean returning past the line
   * that records what was drawn.
   */
  const fold = (current: Attached, sessionId: string, update: SessionUpdate): void => {
    if (update.sessionUpdate === "available_commands_update") {
      const advertised = (update as Record<string, unknown>)["availableCommands"];
      setCommands(Array.isArray(advertised) ? (advertised as AvailableCommand[]) : []);
      return;
    }
    if (update.sessionUpdate === "model_changed") {
      // Broadcast to every subscriber, so a terminal on the same leader — or a
      // second tab — moves this picker too.
      const state = models();
      if (state) setModels(applyModelChanged(state, update as Record<string, unknown>));
      return;
    }
    // The mode is taken from here and *only* here. `session/set_mode` answers
    // `{}` for an id it does not implement as readily as for one it does, so a
    // control that moved on its own request would show a mode the agent is not
    // in. This broadcast is the agent saying it changed — and it reaches every
    // subscriber, so a terminal switching to plan mode moves this too.
    if (update.sessionUpdate === "current_mode_update") {
      const id = readModeUpdate(update as Record<string, unknown>);
      if (id !== null) setSessionMode(id);
      return;
    }
    if (update.sessionUpdate === "interaction_resolved") {
      const id = String((update as Record<string, unknown>)["tool_call_id"] ?? "");
      const pending = permissions.find((p) => p.toolCallId === id);
      pending?.answer({ outcome: { outcome: "cancelled" } });
      return;
    }
    current.subagents.apply(sessionId, update);
    current.transcript.apply(update);
  };

  const connect = async (base: string, secret: string): Promise<void> => {
    client?.close();
    say("connecting…");
    // The session on screen belonged to the socket that just closed. Its
    // transcript, its subagents and its model catalog were built from one
    // leader's replay, and the roster about to arrive is another leader's — so
    // keeping it would draw instance A's conversation under instance B's list
    // of sessions, with nothing on screen saying the two are unrelated.
    // `disconnect` has always cleared this; `connect` never did, which made
    // "hang up, then connect elsewhere" and "connect elsewhere" differ.
    //
    // Re-attaching after a *reconnect* is not lost by this: the caller that
    // wants it reads the session id before it calls here (`Link.resume`), which
    // is also what lets it tell "the link came back" from "we moved".
    //
    // What *is* kept is put aside rather than left in place, which is the whole
    // of the difference. The transcript on screen belongs to the dead socket, so
    // it may not be drawn under the next one's roster — but if the next socket
    // turns out to reach the same machine and the same session, it is also the
    // only copy of the conversation that exists outside the leader, and
    // throwing it away is what made every reconnect a full replay.
    const leaving = attached();
    const left = identity();
    carried =
      leaving && left.agentId
        ? { sessionId: leaving.entry.sessionId, attached: leaving, agentId: left.agentId }
        : null;
    setAttached(null);
    attachedOn = null;
    // Every card on screen was addressed to the socket that just closed, and
    // answering one would write the reply into a socket with nowhere to send
    // it — the agent would stay parked on a question the person believes they
    // have answered. Measured against a live agent: without this, a reconnect
    // stacked a second copy of the same permission beside the dead one.
    //
    // Nothing is lost by dropping them. The leader caches an open interaction
    // and re-sends it to a client that has just attached
    // (`leader/server.rs:2095-2114`), so a permission comes back by itself on
    // the next `session/load`. Folder trust is not cached, and is not kept for
    // that reason either: it was equally unanswerable, and the agent releases
    // its dedup key when the round-trip ends so the next session in that
    // workspace is asked afresh.
    setPermissions([]);
    setFolderTrusts([]);
    setModels(null);
    setSessionInfo(null);
    setDockEnabled(null);
    // The identity belongs to the socket being replaced. Keeping it would let
    // the line above the roster name the machine we have just left.
    setIdentity({});
    // A new socket is a new client on the leader's books, so the search id from
    // the old one is unusable: its status stream is addressed to a client that
    // no longer exists, and a `close` has nowhere to go. Forget it rather than
    // pretend. The orphan on the agent falls to the 300s idle sweep, which runs
    // inside the next `open` — this client's own, the next time anyone types
    // `@`, so the leak closes itself.
    fileSearch.forget();
    const next = new GatewayClient(() => new WebSocket(gatewayUrl(base, secret)));
    client = next;
    next.onNotification(onNotification);
    // Answer, never ignore. `request_permission` has no timeout on the agent
    // side, so silence parks the session actor for every client attached to it.
    // Anything else this client does not implement is refused, which at least
    // releases the caller.
    next.onRequest((method, params) => {
      if (method === "session/request_permission") {
        return askPermission(params as RequestPermissionRequest);
      }
      if (method === "x.ai/folder_trust/request") {
        return askFolderTrust(params as FolderTrustRequest);
      }
      return undefined;
    });

    try {
      await next.connect();
      const initialized = (await next.request("initialize", {
        protocolVersion: PROTOCOL_VERSION,
        clientCapabilities: {
          fs: { readTextFile: false, writeTextFile: false },
          terminal: false,
        },
        clientInfo: { name: "grok-web", version: "0.1.0" },
      })) as InitializeResponse;
      const cwd = initialized._meta?.currentWorkingDirectory;
      if (typeof cwd === "string" && cwd) setAgentCwd(cwd);
      const answered = readIdentity(initialized._meta);
      // Read before the identity is replaced, because it is a comparison
      // between two answers and the old one is about to be gone. This is the
      // only thing on the wire that separates "the socket was away for a
      // moment" from "the process this page was talking to is not running any
      // more" — a distinction the retry ladder cannot make, because both look
      // exactly like a socket that closed and opened again.
      setRelaunch(relaunched(left, answered));
      setIdentity(answered);
      const seed = initialized._meta?.availableCommands;
      setSeedCommands(Array.isArray(seed) ? seed : []);
      setCommands(seedCommands());
      // Before the roster, because this is what decides whether attaching to
      // anything on it can work at all.
      await settleAuth(initialized);
      await refreshRoster();
      await refreshSettings();
      say("connected");
    } catch (e) {
      say(`connection failed: ${String(e)}`);
      throw e;
    }
  };

  /**
   * Move to the login screen, and say so when there is no login to show.
   *
   * A card with a title and no button under it is the shape this avoids: an
   * agent can advertise a method this build cannot drive, or only credentials
   * it reads for itself, and in both cases the person needs the reason and the
   * remedy rather than an empty panel.
   */
  const needLogin = (): void => {
    setAuth(
      produce((state) => {
        state.status = "needed";
        state.blocked = noLoginAvailable(state.methods);
      }),
    );
  };

  /**
   * Settle authentication from what `initialize` said, exactly as the terminal
   * settles it (`eager_auth_or_login_fallback`,
   * `xai-grok-pager/src/acp/mod.rs:735`).
   *
   * The order of these branches is the whole behaviour, and none of it is this
   * client's invention:
   *
   * 1. **No methods at all** is fail-closed by construction — a
   *    `preferred_method` pin with no credential builds an empty list
   *    (`auth_method.rs:173`). There is nothing to drive, so say where it can
   *    be fixed instead.
   * 2. **A restored plugin sign-in** means the agent is already authenticated.
   *    Authenticating on that method would re-drive the plugin's interactive
   *    flow, which is the login the restoration exists to spare the user.
   * 3. **An interactive method first** means `build_auth_methods` found no
   *    credential: every non-interactive one it finds is ordered ahead of the
   *    login. Do not authenticate eagerly — that opens a browser nobody asked
   *    for. Wait for the person.
   * 4. Otherwise authenticate on the agent's own choice.
   */
  const settleAuth = async (initialized: InitializeResponse): Promise<void> => {
    const methods = initialized.authMethods ?? [];
    const defaultId = initialized._meta?.defaultAuthMethodId ?? null;
    setAuth(
      produce((state) => {
        state.methods = methods;
        state.error = null;
        state.blocked = null;
        state.driving = null;
        state.url = null;
        state.mode = null;
      }),
    );

    if (methods.length === 0) {
      needLogin();
      say("cannot sign in: the agent advertised no method");
      return;
    }
    if (initialized._meta?.restoredAuthMeta) {
      setAuth("status", "settled");
      return;
    }
    if (startupNeedsLogin(methods)) {
      needLogin();
      say("sign in to start a session");
      return;
    }

    const eager = selectEagerMethod(methods, defaultId);
    if (!eager) {
      needLogin();
      return;
    }
    try {
      await client?.request("authenticate", { methodId: eager });
      setAuth("status", "settled");
    } catch (e) {
      // The shell owns the fallthrough between non-interactive methods, and a
      // failed api-key authenticate must not be promoted to a browser login —
      // `eager_auth_or_login_fallback` says so, and it can afford to: an
      // advertised `xai.api_key` means `initialize` already installed a default
      // method, so sessions still open. Report it and leave the login alone.
      const advertisesApiKey = methods.some((m) => m.id === XAI_API_KEY);
      setAuth("error", String(e));
      if (advertisesApiKey) setAuth("status", "settled");
      else needLogin();
    }
  };

  /**
   * Ask for the authorize URL until the agent has one.
   *
   * Runs alongside the `authenticate` call, never after it: that call does not
   * return until the whole login has finished, so the URL a person needs in
   * order to finish it can only be collected concurrently. The retries are for
   * the poll that arrives before the attempt is registered; once it is, the
   * first poll blocks until the URL is ready.
   */
  const pollAuthUrl = async (requestSeq: number): Promise<void> => {
    for (let i = 0; i < AUTH_URL_POLLS; i += 1) {
      if (i > 0) await new Promise((resolve) => setTimeout(resolve, AUTH_URL_POLL_GAP_MS));
      if (auth.requestSeq !== requestSeq || !client) return;
      let response: AuthUrlResponse | undefined;
      try {
        response = (await client.ext("x.ai/auth/get_url", {})) as AuthUrlResponse;
      } catch {
        return;
      }
      if (auth.requestSeq !== requestSeq) return;
      const url = response?.auth_url;
      if (!url) continue;
      const mode = response?.mode ?? (response?.external_provider ? "command" : "loopback");
      setAuth(
        produce((state) => {
          state.url = url;
          // `mode` is authoritative; `external_provider` is what an older agent
          // sends instead, and it only ever meant "command".
          state.mode = mode;
        }),
      );
      return;
    }
  };

  /**
   * Start an interactive login on `methodId`, or on the first advertised
   * interactive method when the caller names none.
   *
   * `force_interactive` is what makes this mean "sign in" rather than "use
   * whatever is cached": a person pressed a button. It clears nothing, so a
   * login abandoned halfway leaves every running session on this leader
   * working.
   */
  const login = async (methodId?: string): Promise<void> => {
    if (!client) return;
    const method = methodId
      ? auth.methods.find((candidate) => candidate.id === methodId)
      : interactiveMethods(auth.methods)[0];
    if (!method) {
      setAuth("error", "That sign-in is no longer advertised by the agent.");
      return;
    }
    if (!drivable(method)) {
      setAuth(produce((state) => {
        state.blocked = undrivableMessage(method.id);
        state.status = "needed";
      }));
      return;
    }

    const requestSeq = auth.requestSeq + 1;
    setAuth(
      produce((state) => {
        state.requestSeq = requestSeq;
        state.status = "running";
        state.driving = method.id;
        state.url = null;
        state.mode = startMode(method);
        state.error = null;
        state.blocked = null;
      }),
    );
    say("signing in with " + method.name + "…");

    void pollAuthUrl(requestSeq);
    try {
      (await client.request("authenticate", {
        methodId: method.id,
        _meta: { request_seq: requestSeq, force_interactive: true },
      })) as AuthenticateResponse;
      if (auth.requestSeq !== requestSeq) return;
      setAuth(
        produce((state) => {
          state.status = "settled";
          state.driving = null;
          state.url = null;
          state.mode = null;
        }),
      );
      say("signed in");
      // Both were read before the credential existed.
      await refreshRoster();
      await refreshSettings();
    } catch (e) {
      if (auth.requestSeq !== requestSeq) return;
      setAuth(
        produce((state) => {
          state.status = "needed";
          state.driving = null;
          state.url = null;
          state.mode = null;
          state.error = String(e);
        }),
      );
      say("sign-in failed");
    }
  };

  /**
   * Hand back a pasted callback URL or bare code.
   *
   * Only meaningful in `loopback` mode, which is why the card offers the box
   * nowhere else: that is the one flow racing a pasted code against the
   * callback listener (`oidc/login.rs`, `race_callback_and_client_ui`). The
   * agent accepts either the whole `http://127.0.0.1:PORT/callback?code=…`
   * address or the bare code (`parse_pasted_input`), so neither form has to be
   * explained to the person pasting.
   *
   * A paste the agent cannot parse at all — empty, or a URL with no `code` —
   * is dropped by its bridge and the flow keeps waiting, so nothing is reported
   * here. A paste that parses but is not a valid code is a different thing: it
   * is exchanged, refused by the issuer, and ends the attempt. That failure
   * arrives as the `authenticate` call's own error and lands on the card in the
   * issuer's words. Verified by pasting a bogus code at a live issuer.
   */
  const submitAuthCode = async (code: string): Promise<void> => {
    if (!client || !code.trim()) return;
    try {
      await client.ext("x.ai/auth/submit_code", { code: code.trim() });
      say("code submitted");
    } catch (e) {
      setAuth("error", String(e));
    }
  };

  /**
   * Abandon the login in flight.
   *
   * Scoped by `request_seq` so a cancel that arrives late cannot tear down a
   * login that has already replaced this one — the agent's single flight keys
   * on the same number (`cancel_for_client_seq`). Bumping the sequence first is
   * what makes this client discard the abandoned attempt's own results too.
   */
  const cancelLogin = (): void => {
    const requestSeq = auth.requestSeq;
    setAuth(
      produce((state) => {
        state.requestSeq = requestSeq + 1;
        state.status = "needed";
        state.driving = null;
        state.url = null;
        state.mode = null;
      }),
    );
    void client?.ext("x.ai/auth/cancel", { request_seq: requestSeq }).catch(() => undefined);
    say("sign-in cancelled");
  };

  const refreshRoster = async (): Promise<void> => {
    if (!client) return;
    const response = (await client.ext("x.ai/sessions/list", {})) as RosterListResponse;
    roster.replace(response.sessions ?? []);
  };

  /**
   * Read the settings catalog and keep the rows the wire says a non-terminal
   * client should draw. Read-only: writing is `x.ai/settings/set`, and that is
   * not what this client is proving.
   */
  const refreshSettings = async (): Promise<void> => {
    if (!client) return;
    const response = (await client.ext("x.ai/settings/list", {})) as SettingsListResponse;
    const all = response.catalog?.rows ?? [];
    const rows = visibleSettingRows(all);
    setSettings(
      produce((state) => {
        state.rows = rows;
        state.terminalOnly = all.length - rows.length;
        state.values = response.state?.values ?? {};
        state.locks = response.state?.locks ?? {};
      }),
    );
  };

  /**
   * Read the attached session's info, and with it the resolved context window.
   *
   * **Asked, never streamed.** `contextFacts` rides `x.ai/session/info`, a
   * request/response method with no notification carrier of its own — the pager
   * debounces its own asking (`app/agent_view/mod.rs:456-459`) rather than
   * subscribing to anything. So this runs on attach, at the end of a turn and
   * when a person presses the button, and never on a timer: a poll would be
   * this client inventing a cadence the product has not got, and the number it
   * would be polling for only changes when a turn does.
   *
   * The answer is dropped if the session moved while it was in flight. The
   * handler replies `{}` for a session it has no resident handle for, which is
   * a real answer and is stored as one: it is how "the leader no longer holds
   * this session" reaches the widget.
   */
  const refreshSessionInfo = async (): Promise<void> => {
    const current = attached();
    if (!client || !current) return;
    const asked = current.entry.sessionId;
    try {
      const response = (await client.ext("x.ai/session/info", {
        sessionId: asked,
      })) as SessionInfoResponse;
      if (attached()?.entry.sessionId !== asked) return;
      setSessionInfo(response ?? {});
    } catch (e) {
      if (attached()?.entry.sessionId !== asked) return;
      say(`could not read the context window: ${String(e)}`);
    }
  };

  /**
   * List one directory for the picker.
   *
   * No `sessionId` is sent, on purpose: the agent consults it only to resolve a
   * *relative* path against that session's cwd, and every path this client
   * holds is absolute. The walk of an absolute path is identical whichever
   * session asks — which is why a picker needed no wire change at all.
   */
  const listDirectory = async (path: string): Promise<FsListResponse> => {
    if (!client) return { nodes: [], truncated: false };
    const response = (await client.ext("x.ai/fs/list", {
      path,
      ...LIST_PARAMS,
    })) as FsListResponse;
    return { nodes: response.nodes ?? [], truncated: response.truncated ?? false };
  };

  /**
   * Does the path exist?
   *
   * Asked only when a listing comes back empty, because that is the one case
   * `fs/list` cannot explain by itself: an empty directory, a path that is a
   * file, and a directory the leader may not read all answer with the same
   * empty page.
   */
  const pathExists = async (path: string): Promise<boolean> => {
    if (!client) return false;
    const response = (await client.ext("x.ai/fs/exists", { path })) as FsExistsResponse;
    return response.exists === true;
  };

  /** A session nothing is known about yet. */
  const freshAttachment = (entry: RosterEntry): Attached => {
    const transcript = createTranscript();
    return {
      entry,
      transcript,
      subagents: createSubagents(entry.sessionId),
      tasks: createTasks(),
      // The transcript's own position, not a second count kept beside it: the
      // mark has to be what the transcript will be rewound to, and two numbers
      // that are supposed to be equal are a thing that can stop being equal.
      resumption: createResumption(() => transcript.mark()),
    };
  };

  const attach = async (entry: RosterEntry): Promise<void> => {
    if (!client) return;
    // Already on this session, on this socket: asking again would replay the
    // transcript a second time into the transcript the first ask is still
    // filling. See {@link attachedOn}. Set before the first `await`, so two
    // callers in one turn cannot both get past it.
    if (attachedOn?.client === client && attachedOn.sessionId === entry.sessionId) return;
    const asking = client;
    attachedOn = { client: asking, sessionId: entry.sessionId };
    // The search is rooted in the session being left, so it goes back to the
    // agent now, while there is still a socket to say so on. Nothing else will
    // ever tell it: the workspace has no hook on a client going away.
    fileSearch.release();
    // The same session on the same machine, on the socket that replaced the one
    // it was on: keep the conversation and ask only for what happened while it
    // was gone. Anything else — another session, another machine, a first
    // attach — starts empty, which is what every attach used to do.
    const held = takeCarried(entry.sessionId);
    // A carry-over with nothing addressable in it is not a resume. It happens
    // for a session whose only event so far is half of one message: the agent
    // cannot be asked to continue from a chunk it never wrote as a line, so
    // there is nothing to keep and starting empty is the honest form of it.
    const resumed = held && held.resumption.cursor() !== null ? held : null;
    // The roster row is taken fresh even when everything else is kept: a title,
    // an activity and even a cwd can have moved while the link was down, and
    // the row is the leader's current answer about all three.
    const current: Attached = resumed ? { ...resumed, entry } : freshAttachment(entry);
    const subagents = current.subagents;
    // Sent only on a resume. On a first attach there is nothing to be after,
    // and the agent reads an absent cursor and a stale one the same way — a
    // full replay — so this is about honesty rather than about the outcome.
    const cursor = resumed ? current.resumption.cursor() : null;
    // The cursor is behind the screen by whatever was still streaming when the
    // link died, and the tail will send that back whole. Bringing the screen
    // down to the cursor is what keeps the two from being drawn on top of each
    // other; it can only remove what is about to be replaced.
    if (resumed) current.transcript.rewind(current.resumption.mark());
    current.resumption.loading(cursor);
    setAttached(current);
    // Back to the pre-session builtins until this session advertises its own.
    // `session/load` triggers that advertisement, so the gap is one round-trip
    // wide — and during it the menu offers only what every session has.
    setCommands(seedCommands());
    // The catalog belongs to the session being left, so it goes with it. Showing
    // the previous session's current model over a session still loading would be
    // a wrong answer where "not yet" is the true one. The context window is the
    // same case and worse: a breakdown is nothing but numbers, so a stale one
    // reads as this session's own.
    setModels(null);
    setSessionInfo(null);
    say(cursor === null ? `loading ${entry.sessionId}…` : `resuming ${entry.sessionId}…`);
    try {
      // `cwd` comes straight off the roster row. That it is there at all is the
      // reason a second client can attach to a session it did not create.
      const loaded = await client.request("session/load", {
        sessionId: entry.sessionId,
        cwd: entry.cwd,
        mcpServers: [],
        // Omitted rather than sent as null on a first attach. The agent reads
        // the key's absence and an unresolvable value identically, but the two
        // are different statements and one of them is a lie.
        ...(cursor === null ? {} : { _meta: { cursor } }),
      });
      // This reply used to be discarded whole, which is the only reason the
      // model picker looked like it needed a wire change: `models` is on it.
      setModels(readModelState(loaded));
      // Everything the load was going to send has been sent: the agent drains
      // its replay before it answers (`agent/mvp_agent/replay.rs:243-249`), so
      // by here the count is final and so is which of the two answers it was.
      const arrived = current.resumption.loaded();
      if (resumed && relaunch()) current.transcript.notice(RESTARTED_NOTICE);
      if (arrived.rebuilt) current.transcript.notice(REBUILT_NOTICE);
      say(describeArrival(entry.sessionId, cursor !== null, arrived));
    } catch (e) {
      // Nothing was replayed, so nothing is on this socket to be replayed
      // twice: release the claim rather than leaving the session unattachable
      // until the link is rebuilt.
      if (attachedOn?.client === asking) attachedOn = null;
      // Close the window the request opened. A load that failed still leaves
      // this expecting the replay it asked for, and an expectation left open is
      // a licence for a stray replayed frame to wipe the transcript.
      current.resumption.loaded();
      say(`load failed: ${String(e)}`);
      return;
    }
    await seedRunningSubagents(entry.sessionId, subagents);
    // Not awaited, unlike the seed above it. That one fills counters that would
    // otherwise read "unknown" on rows already on screen; this one only
    // *corrects* an elapsed time the replay could not date, and a leader too
    // old to know `x.ai/task/list` would never answer at all — which awaiting
    // would turn into a session that never finishes attaching.
    void seedTasks(entry.sessionId, current.tasks);
    // Not awaited, and this is the same argument as the seed above it: the
    // transcript is what attaching is, and a window that has not arrived yet is
    // a widget that is not on screen yet. Waiting on it would let a leader slow
    // to answer one extension method hold up the session that is already
    // loaded.
    void refreshSessionInfo();
  };

  /**
   * Ask which children are running right now.
   *
   * The replay a `session/load` performs carries every `subagent_spawned` and
   * `subagent_finished` this session ever wrote, so *which* children exist is
   * already known by the time this runs. What replay cannot carry is progress:
   * those ticks are deliberately never persisted (`agent/subagent/mod.rs:2049`),
   * so a client attaching into a live fan-out would show counters and an
   * elapsed time of "unknown" until each child's next tick — two seconds for a
   * busy child, eight for a quiet one.
   *
   * `x.ai/subagent/list_running` is the answer the shell itself names for this
   * case (`agent/subagent/mod.rs:2050`). A failure is not reported: the stream
   * fills the same fields a moment later, so the only cost is the wait this
   * call existed to skip.
   */
  const seedRunningSubagents = async (sessionId: string, subagents: Subagents): Promise<void> => {
    if (!client) return;
    try {
      const response = (await client.ext("x.ai/subagent/list_running", {
        sessionId,
      })) as ListRunningSubagentsResponse;
      subagents.seed(sessionId, response?.subagents ?? []);
    } catch {
      // See above: the stream reports the same thing, only later.
    }
  };

  /**
   * Stop a running child.
   *
   * Destructive and not undoable: the child's turn is cancelled where it
   * stands, and whatever it had done is not handed back to the parent. The
   * card arms the button before it sends, which is this client's own decision
   * and is argued in the README.
   *
   * The three outcomes differ in one way that matters: only `cancelled` is
   * followed by a `subagent_finished`. For `already_finished` and `not_found`
   * nothing more is coming, so the row would sit marked "stopping…" forever if
   * this waited for an event — which is why the pager finalizes those two
   * itself (`app/effects/helpers.rs:1176`) and so does this.
   */
  const cancelSubagent = async (subagentId: string): Promise<void> => {
    const current = attached();
    if (!client || !current) return;
    current.subagents.markKillSent(subagentId, performance.now());
    say(`stopping ${subagentId}…`);
    let response: CancelSubagentResponse | undefined;
    try {
      response = (await client.ext("x.ai/subagent/cancel", {
        sessionId: current.entry.sessionId,
        subagentId,
      })) as CancelSubagentResponse;
    } catch (e) {
      // The RPC failed, so the child may well still be running. Clear the mark
      // and leave the row alone rather than reporting a stop that never
      // happened — the pager's `RpcFailed` rule (`effects/helpers.rs:1177`).
      current.subagents.clearKill(subagentId);
      say(`could not stop ${subagentId}: ${String(e)}`);
      return;
    }
    // `outcome` is the typed answer; `cancelled` is what an older shell sends
    // instead, and it only ever means "a live child was stopped".
    const kind = response?.outcome?.kind ?? (response?.cancelled ? "cancelled" : "not_found");
    if (kind === "cancelled") {
      say(`stopping ${subagentId}; waiting for it to finish`);
      return;
    }
    // Nothing is coming, so this client finalizes the row itself. An
    // `already_finished` carries the real terminal status. A `not_found` does
    // not: it means the agent has no record of this id, which is not evidence
    // that anything was cancelled. The pager substitutes `"cancelled"` there
    // (`app/dispatch/task_result.rs:780`); this client says `"unknown"`
    // instead, and the README argues why.
    const status =
      response?.outcome?.kind === "already_finished"
        ? ((response.outcome as { status?: string }).status ?? "unknown")
        : "unknown";
    current.subagents.finalize(subagentId, status);
    say(
      kind === "not_found"
        ? `the agent has no record of ${subagentId}`
        : `${subagentId} had already finished: ${status}`,
    );
  };

  /**
   * Ask what background work is running right now.
   *
   * The same shape as {@link seedRunningSubagents} and for the same reason.
   * `session/load` replays every `task_backgrounded` this session ever wrote,
   * so *which* commands exist is known by the time this runs; what the replay
   * cannot carry is when any of them started, because the notification has no
   * timestamp on it. The pager lives with that — it stamps its own clock on a
   * replayed task (`acp_handler/background.rs:157`), which on a resume dates
   * every one of them to the moment of resuming — and `x.ai/task/list` is the
   * agent's own answer (`extensions/task.rs:394`).
   *
   * A failure is not reported. The rows are already on screen from the replay;
   * only the elapsed column stays an em dash, which is the true statement.
   */
  const seedTasks = async (sessionId: string, tasks: Tasks): Promise<void> => {
    if (!client) return;
    try {
      const response = (await client.ext("x.ai/task/list", { sessionId })) as ListTasksResponse;
      tasks.seed(sessionId, response?.tasks ?? []);
    } catch {
      // See above: the stream said which, and only the when is missing.
    }
  };

  /**
   * Kill a background command or a monitor.
   *
   * Destructive: the process is signalled where it stands, and whatever it had
   * not finished is not finished. The rail arms the button before it sends.
   *
   * The three outcomes differ the way `x.ai/subagent/cancel`'s do, and the
   * pager branches on them in the same three ways (`app/dispatch/turn.rs:786-817`).
   * Only `killed` is followed by a `task_completed`, so only it leaves the row
   * marked. `already_exited` means the completion has already been sent.
   * `not_found` means the agent has no such task at all — a row replayed out of
   * a session whose process died with it — and the row goes, because a stop
   * button over nothing is worse than no row.
   */
  const killTask = async (taskId: string): Promise<void> => {
    const current = attached();
    if (!client || !current) return;
    current.tasks.markKillSent(taskId, performance.now());
    say(`stopping ${taskId}…`);
    let response: KillTaskResponse | undefined;
    try {
      response = (await client.ext("x.ai/task/kill", {
        sessionId: current.entry.sessionId,
        taskId,
        // A browser is a client UI, which is also the field's default. Sent
        // anyway because the other value means bulk teardown, and the two
        // differ in whether the model is told its command was killed
        // (`computer/types.rs:309-315`).
        source: "clientUi",
      })) as KillTaskResponse;
    } catch (e) {
      // The RPC failed, so the process may well still be running. Clear the
      // mark and leave the row rather than reporting a kill that never
      // happened — the pager's `BgTaskKillFailed` rule (`turn.rs:809-816`).
      current.tasks.clearKill(taskId);
      say(`could not stop ${taskId}: ${String(e)}`);
      return;
    }
    const outcome = response?.outcome;
    if (outcome === "killed") {
      say(`stopping ${taskId}; waiting for it to exit`);
      return;
    }
    if (outcome === "not_found") {
      current.tasks.forget(taskId);
      say(`the agent has no record of ${taskId}`);
      return;
    }
    // `already_exited`, and also an outcome this client does not know: clear
    // the mark and keep the row, which is what the pager does with an
    // unparseable answer for the same reason — it is not evidence of anything.
    current.tasks.clearKill(taskId);
    say(outcome === "already_exited" ? `${taskId} had already exited` : `stopped ${taskId}`);
  };

  /**
   * Delete a scheduled `/loop`.
   *
   * Removed from the list before the agent answers, which is the pager's own
   * optimism (`app/dispatch/turn.rs:684-696`): a schedule is a record rather
   * than a process, deleting one cannot half-succeed, and the
   * `scheduled_task_deleted` broadcast that follows lands on a row already
   * gone. A failure puts it back, because a schedule still on the agent's books
   * that this browser has stopped showing is the one outcome worth a word.
   */
  const cancelScheduledLoop = async (taskId: string): Promise<void> => {
    const current = attached();
    if (!client || !current) return;
    const removed = current.tasks.loops.find((loop) => loop.taskId === taskId);
    current.tasks.removeLoop(taskId);
    say(`removing ${taskId}…`);
    try {
      const response = (await client.ext("x.ai/scheduler/delete", {
        sessionId: current.entry.sessionId,
        taskId,
      })) as DeleteScheduledTaskResponse;
      say(response?.deleted === false ? `the agent had no schedule ${taskId}` : `removed ${taskId}`);
    } catch (e) {
      if (removed) {
        current.tasks.apply(
          {
            sessionUpdate: "scheduled_task_created",
            task_id: removed.taskId,
            prompt: removed.prompt,
            human_schedule: removed.humanSchedule,
            next_fire_at: removed.nextFireAt ?? null,
          } as unknown as SessionUpdate,
          false,
        );
      }
      say(`could not remove ${taskId}: ${String(e)}`);
    }
  };

  /**
   * Create a session rooted at `cwd` and attach to it.
   *
   * `cwd` is a parameter of `session/new`, never process state, which is why a
   * browser can put a session anywhere the leader can reach — and why "switch
   * instance" is "attach to another session", not "repoint this one".
   */
  const createSession = async (cwd: string): Promise<string | null> => {
    if (!client) return null;
    say(`creating a session in ${cwd}…`);
    try {
      const created = (await client.request("session/new", {
        cwd,
        mcpServers: [],
      })) as NewSessionResponse;
      // Same catalog, same field: `session/new` builds its reply with
      // `.models(...)` too (`session_setup.rs:751-754`). Navigating to the new
      // session attaches and replaces this, so it only ever fills the gap.
      setModels(readModelState(created));
      await refreshRoster();
      say(`created ${created.sessionId}`);
      return created.sessionId;
    } catch (e) {
      say(`could not create a session: ${String(e)}`);
      return null;
    }
  };

  /**
   * Ask the agent to enter a session mode.
   *
   * Nothing local changes here on purpose. `session/set_mode` answers `{}`
   * whether or not the id means anything to the agent — `ask`, `acceptEdits`
   * and a nonsense string were all accepted with no effect — so the only
   * trustworthy signal that a mode was entered is the `current_mode_update` the
   * agent broadcasts back. See `modes.ts`.
   */
  const setSessionModeRequest = async (modeId: string): Promise<void> => {
    const current = attached();
    if (!client || !current) return;
    try {
      await client.request("session/set_mode", { sessionId: current.entry.sessionId, modeId });
    } catch (e) {
      say(`mode change failed: ${String(e)}`);
    }
  };

  /**
   * Set how much the agent decides for itself.
   *
   * A notification rather than a request, because that is what the pager sends:
   * the leader fans it out to the matching sessions and there is nothing to
   * answer. It is a different axis from the session mode above and rides a
   * different method — `modes.ts` says why that distinction is load-bearing.
   */
  const setPermissionMode = (mode: PermissionMode): void => {
    if (!client) return;
    client.notify("_x.ai/yolo_mode_changed", permissionModeParams(mode));
    say(`permission mode: ${mode}`);
  };

  const prompt = async (
    text: string,
    /**
     * Files attached to this turn, already turned into blocks.
     *
     * Appended after the text, which is the order the terminal builds the same
     * array in — `interjection.rs:89` extends the blocks with its pasted images
     * once the typed text is in. What a given file *becomes* is a decision with
     * its own measurements behind it, so it is made in `attach.ts` and arrives
     * here already settled.
     */
    attachments: readonly ContentBlock[] = [],
  ): Promise<void> => {
    const current = attached();
    if ((!text && attachments.length === 0) || !client || !current) return;
    const driving = client;
    say("running…");
    try {
      const response = (await driving.request("session/prompt", {
        sessionId: current.entry.sessionId,
        prompt: [...(text ? [{ type: "text", text } as ContentBlock] : []), ...attachments],
        // The agent echoes `promptId` on every notification it emits for this
        // turn, which is how a client tells a cancelled turn's chunks from the
        // next turn's. Not used for filtering yet, but omitting it would throw
        // the information away at the source.
        _meta: { promptId: crypto.randomUUID() },
      })) as PromptResponse;
      say(`turn ended: ${response.stopReason}`);
      // The window moves when a turn does, which is why the end of one is the
      // cadence and there is no timer beside it. Not awaited: the turn is over
      // either way, and the composer must not wait on a number.
      void refreshSessionInfo();
    } catch (e) {
      // A socket that closed is not a turn that failed. `session/prompt` is
      // rejected along with every other request in flight when the link goes
      // (`failAllPending`, `client.ts`), but the agent is not listening to that
      // socket to decide whether to keep working — it goes on running the turn,
      // writes every delta to the session's log, and hands them over on the
      // next attach. Calling that a failure is the one lie this page can tell
      // that a person cannot check.
      if (driving.linkState() === "closed") {
        say("connection lost; the agent is still running this turn");
        return;
      }
      say(`prompt failed: ${String(e)}`);
    }
  };

  /**
   * Switch the attached session's model, and — for a switch that has no effort
   * with it — remember it as the default.
   *
   * **Two calls, not one, and sending either alone diverges from the terminal.**
   * `/model <name>` there emits both `Effect::PersistSetting { key:
   * "default_model" }` and `Effect::SwitchModel`
   * (`pager/src/app/dispatch/settings/setters.rs:1776-1801`), while the
   * shell-side setter behind that key only writes the file
   * (`xai-grok-shell/src/util/config/settings_apply.rs:233-240`). So a client
   * that writes the setting alone saves a preference and leaves the live session
   * on the old model — contradicting the row's own description, which promises
   * "Changing this also switches the active session".
   *
   * `effort` is the other case and must NOT be persisted: it is session-scoped
   * and rides in `_meta` on the same `set_model` (`app/effects/mod.rs:1846-1873`),
   * which is why `persist` follows from whether an effort was picked rather than
   * being a second choice offered to the caller.
   *
   * The switch goes first. If it fails there is nothing to remember, and the
   * write would otherwise leave the next session starting on a model this one
   * refused.
   */
  const setModel = async (modelId: string, effort?: string | null): Promise<void> => {
    const current = attached();
    if (!client || !current) return;
    const sessionId = current.entry.sessionId;
    try {
      await client.request("session/set_model", setModelRequest(sessionId, modelId, effort));
    } catch (e) {
      say(`could not switch model: ${String(e)}`);
      return;
    }
    // The agent broadcasts `model_changed` to every subscriber including this
    // one, but only when the session actor gets there; applying the answer to
    // this client's own request keeps the list from lagging its own click.
    const state = models();
    if (state) {
      setModels(
        applyModelChanged(state, { model_id: modelId, ...(effort ? { reasoning_effort: effort } : {}) }),
      );
    }
    if (effort) {
      say(`switched to ${modelId} at ${effort} for this session`);
      return;
    }
    let response: { applied?: boolean; refusal?: { message?: string } } | undefined;
    try {
      response = (await client.ext("x.ai/settings/set", defaultModelWrite(sessionId, modelId))) as {
        applied?: boolean;
        refusal?: { message?: string };
      };
    } catch (e) {
      say(`switched to ${modelId}, but could not remember it: ${String(e)}`);
      return;
    }
    // A refusal is not a failure: a `config.toml` grok was not given to rewrite
    // is exactly what `update_config`'s `stat` is there to protect, and the
    // sentence naming the file is the shell's, not this client's.
    say(
      response?.applied === false
        ? `switched to ${modelId}; not remembered: ${response.refusal?.message ?? "declined"}`
        : `switched to ${modelId}`,
    );
  };

  const panelAction = async (plugin: string, action: PanelAction): Promise<void> => {
    const current = attached();
    if (!client || !current) return;
    try {
      const response = (await client.ext("x.ai/plugins/panel_action", {
        sessionId: current.entry.sessionId,
        plugin,
        panelId: action.panelId,
        buttonId: action.buttonId,
        inputs: action.inputs,
      })) as PanelActionResponse;
      // `delivered` means handed to the sidecar, not acted on. `false` means the
      // session or the plugin is gone, so the panel on screen is stale.
      if (!response.delivered) {
        say(`panel action not delivered: ${plugin}/${action.panelId}`);
      }
    } catch (e) {
      say(`panel action failed: ${String(e)}`);
    }
  };

  const disconnect = (): void => {
    // Before the socket goes, not after: this is the last moment a `close` can
    // reach the agent, and `release` writes the frame synchronously.
    fileSearch.release();
    client?.close();
    client = null;
    setAttached(null);
    fileSearch.forget();
    // A new connection re-reads the advertised methods and re-runs the eager
    // authenticate. Keeping the old list would offer a login on an agent that
    // may no longer advertise it.
    setAuth(
      produce((state) => {
        state.status = "unknown";
        state.methods = [];
        state.driving = null;
        state.url = null;
        state.mode = null;
        state.error = null;
        state.blocked = null;
      }),
    );
    setSeedCommands([]);
    setCommands([]);
    say("not connected");
  };

  return {
    status,
    attached,
    agentCwd,
    identity,
    dockEnabled,
    commands,
    models,
    roster,
    fileSearch,
    settings,
    sessionInfo,
    auth,
    permissions,
    folderTrusts,
    connect,
    disconnect,
    login,
    submitAuthCode,
    cancelLogin,
    attach,
    createSession,
    listDirectory,
    pathExists,
    prompt,
    cancelSubagent,
    killTask,
    cancelScheduledLoop,
    panelAction,
    setModel,
    sessionMode,
    setSessionMode: setSessionModeRequest,
    setPermissionMode,
    refreshRoster,
    refreshSessionInfo,
  };
}

export type Gateway = ReturnType<typeof createGateway>;
