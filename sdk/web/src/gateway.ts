// The live connection, as reactive state.
//
// Everything the wire teaches is imported, not restated: `client.ts` owns the
// socket and the JSON-RPC framing (including the `_` prefix and the
// inconsistently wrapped extension replies), `wire.ts` owns the shapes. This
// module is only the part that has to be reactive — what is connected, what is
// attached, and what is waiting on an answer.
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
import type { PanelAction } from "./panel.ts";
import { createRoster, type Roster } from "./roster.ts";
import { createTranscript, type Transcript } from "./transcript.ts";
import { LIST_PARAMS, ROOT } from "./directory.ts";
import {
  PROTOCOL_VERSION,
  visibleSettingRows,
  FOLDER_TRUST_DISMISSED,
  type AuthMethod,
  type AuthUrlMode,
  type AuthUrlResponse,
  type AuthenticateResponse,
  type AvailableCommand,
  type FolderTrustOutcome,
  type FolderTrustRequest,
  type FolderTrustResponse,
  type FsExistsResponse,
  type FsListResponse,
  type InitializeResponse,
  type NewSessionResponse,
  type PanelActionResponse,
  type PromptResponse,
  type RequestPermissionRequest,
  type RequestPermissionResponse,
  type RosterChanged,
  type RosterEntry,
  type RosterListResponse,
  type SessionNotification,
  type SettingRow,
  type SettingsListResponse,
} from "./wire.ts";

export type Connection = "offline" | "connecting" | "connected" | "failed";

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
}

const STORE_KEYS = { url: "grok-gateway", secret: "grok-secret", theme: "grok-theme" } as const;

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
  const [connection, setConnection] = createSignal<Connection>("offline");
  const [status, setStatus] = createSignal("not connected");
  const [attached, setAttached] = createSignal<Attached | null>(null);
  // Where the directory picker starts walking. The agent names its own launch
  // directory in `initialize`'s `_meta`; the root is the fallback because it is
  // the one path that always exists, and it is never a *limit* — `session/new`
  // takes any absolute `cwd`, so the picker may leave in either direction.
  const [agentCwd, setAgentCwd] = createSignal(ROOT);
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
  const [permissions, setPermissions] = createStore<PendingPermission[]>([]);
  const [folderTrusts, setFolderTrusts] = createStore<PendingFolderTrust[]>([]);
  const roster: Roster = createRoster();

  let client: GatewayClient | null = null;

  const say = (text: string): void => {
    setStatus(text);
  };

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
      if (notification.sessionId !== current.entry.sessionId) return;
      const update = notification.update;
      if (update.sessionUpdate === "available_commands_update") {
        const advertised = (update as Record<string, unknown>)["availableCommands"];
        setCommands(Array.isArray(advertised) ? (advertised as AvailableCommand[]) : []);
        return;
      }
      if (update.sessionUpdate === "interaction_resolved") {
        const id = String((update as Record<string, unknown>)["tool_call_id"] ?? "");
        const pending = permissions.find((p) => p.toolCallId === id);
        pending?.answer({ outcome: { outcome: "cancelled" } });
        return;
      }
      current.transcript.apply(update);
    }
  };

  const connect = async (base: string, secret: string): Promise<void> => {
    client?.close();
    setConnection("connecting");
    say("connecting…");
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
      const seed = initialized._meta?.availableCommands;
      setSeedCommands(Array.isArray(seed) ? seed : []);
      setCommands(seedCommands());
      // Before the roster, because this is what decides whether attaching to
      // anything on it can work at all.
      await settleAuth(initialized);
      await refreshRoster();
      await refreshSettings();
      setConnection("connected");
      say("connected");
    } catch (e) {
      setConnection("failed");
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

  const attach = async (entry: RosterEntry): Promise<void> => {
    if (!client) return;
    setAttached({ entry, transcript: createTranscript() });
    // Back to the pre-session builtins until this session advertises its own.
    // `session/load` triggers that advertisement, so the gap is one round-trip
    // wide — and during it the menu offers only what every session has.
    setCommands(seedCommands());
    say(`loading ${entry.sessionId}…`);
    try {
      // `cwd` comes straight off the roster row. That it is there at all is the
      // reason a second client can attach to a session it did not create.
      await client.request("session/load", {
        sessionId: entry.sessionId,
        cwd: entry.cwd,
        mcpServers: [],
      });
      say(`attached to ${entry.sessionId}`);
    } catch (e) {
      say(`load failed: ${String(e)}`);
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
      await refreshRoster();
      say(`created ${created.sessionId}`);
      return created.sessionId;
    } catch (e) {
      say(`could not create a session: ${String(e)}`);
      return null;
    }
  };

  const prompt = async (text: string): Promise<void> => {
    const current = attached();
    if (!text || !client || !current) return;
    say("running…");
    try {
      const response = (await client.request("session/prompt", {
        sessionId: current.entry.sessionId,
        prompt: [{ type: "text", text }],
        // The agent echoes `promptId` on every notification it emits for this
        // turn, which is how a client tells a cancelled turn's chunks from the
        // next turn's. Not used for filtering yet, but omitting it would throw
        // the information away at the source.
        _meta: { promptId: crypto.randomUUID() },
      })) as PromptResponse;
      say(`turn ended: ${response.stopReason}`);
    } catch (e) {
      say(`prompt failed: ${String(e)}`);
    }
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
    client?.close();
    client = null;
    setAttached(null);
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
    setConnection("offline");
    say("not connected");
  };

  return {
    connection,
    status,
    attached,
    agentCwd,
    commands,
    roster,
    settings,
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
    panelAction,
    refreshRoster,
  };
}

export type Gateway = ReturnType<typeof createGateway>;
