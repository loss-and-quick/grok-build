// The live connection, as reactive state.
//
// Everything the wire teaches is imported, not restated: `client.ts` owns the
// socket and the JSON-RPC framing (including the `_` prefix and the
// inconsistently wrapped extension replies), `wire.ts` owns the shapes. This
// module is only the part that has to be reactive — what is connected, what is
// attached, and what is waiting on an answer.
import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";

import { GatewayClient, gatewayUrl } from "./client.ts";
import type { PanelAction } from "./panel.ts";
import { createRoster, type Roster } from "./roster.ts";
import { createTranscript, type Transcript } from "./transcript.ts";
import {
  PROTOCOL_VERSION,
  visibleSettingRows,
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
  const [settings, setSettings] = createStore<{ rows: SettingRow[]; terminalOnly: number; values: Record<string, unknown>; locks: Record<string, { reason: string }> }>({
    rows: [],
    terminalOnly: 0,
    values: {},
    locks: {},
  });
  const [permissions, setPermissions] = createStore<PendingPermission[]>([]);
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
    next.onRequest((method, params) =>
      method === "session/request_permission"
        ? askPermission(params as RequestPermissionRequest)
        : undefined,
    );

    try {
      await next.connect();
      await next.request("initialize", {
        protocolVersion: PROTOCOL_VERSION,
        clientCapabilities: {
          fs: { readTextFile: false, writeTextFile: false },
          terminal: false,
        },
        clientInfo: { name: "grok-web", version: "0.1.0" },
      });
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

  const attach = async (entry: RosterEntry): Promise<void> => {
    if (!client) return;
    setAttached({ entry, transcript: createTranscript() });
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
    setConnection("offline");
    say("not connected");
  };

  return {
    connection,
    status,
    attached,
    roster,
    settings,
    permissions,
    connect,
    disconnect,
    attach,
    createSession,
    prompt,
    panelAction,
    refreshRoster,
  };
}

export type Gateway = ReturnType<typeof createGateway>;
