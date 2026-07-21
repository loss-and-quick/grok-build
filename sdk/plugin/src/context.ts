// `PluginContext`: the object every hook and `setup()` receives. Thin,
// typed wrapper over `HostClient` plus the static bits handed over at
// `initialize` time (`workspaceRoot`, `sessionId`).

import type { HostClient } from "./rpc.ts";
import type { InitializeParams } from "./generated/InitializeParams.ts";
import type { LogLevelDto } from "./generated/LogLevelDto.ts";
import type { AgentSpawnParams } from "./generated/AgentSpawnParams.ts";
import type { AgentWaitResult } from "./generated/AgentWaitResult.ts";
import type { AgentEventsResult } from "./generated/AgentEventsResult.ts";
import type { AgentCancelOutcomeDto } from "./generated/AgentCancelOutcomeDto.ts";

export interface PluginLogger {
  debug(message: string, fields?: unknown): void;
  info(message: string, fields?: unknown): void;
  warn(message: string, fields?: unknown): void;
  error(message: string, fields?: unknown): void;
}

export interface PluginStorage {
  get(key: string): Promise<unknown>;
  set(key: string, value: unknown): Promise<void>;
  delete(key: string): Promise<boolean>;
  list(prefix?: string): Promise<string[]>;
}

/**
 * Subagent orchestration (`ctx.agents`). Spawned subagents are real children
 * of the plugin's session — the same coordinator, TUI visibility, and
 * cancellation as the model's Task tool. In sessions without orchestration
 * wiring every call rejects with JSON-RPC `method_not_found` (-32601);
 * feature-detect by catching the first call's error.
 */
export interface PluginAgents {
  /** Spawns a subagent; resolves with its id. Validation failures (unknown
   * type, bad model) surface as the terminal result of `wait()`. */
  spawn(spec: AgentSpawnParams): Promise<string>;
  /** Waits up to `timeoutMs` (default 30 000) for the terminal result; a
   * still-running subagent resolves with `status: "running"`. */
  wait(id: string, timeoutMs?: number): Promise<AgentWaitResult>;
  /** Cursor-based progress poll: pass the last `next_cursor` (start at 0);
   * `timeoutMs` (default 0) long-polls until a new event or the deadline.
   * Stop polling once `done` is true. */
  events(id: string, cursor?: number, timeoutMs?: number): Promise<AgentEventsResult>;
  /** Spawnable agent types for this session (sorted, config-filtered). */
  list(): Promise<string[]>;
  /** Cancels a subagent spawned by this plugin. */
  cancel(id: string): Promise<AgentCancelOutcomeDto>;
}

export interface PluginContext {
  readonly workspaceRoot: string;
  readonly sessionId: string;
  readonly log: PluginLogger;
  readonly storage: PluginStorage;
  readonly agents: PluginAgents;
  /** Fetches the plugin's config from the manifest/settings via `config_get`. */
  config<T = unknown>(): Promise<T>;
}

function createLogger(host: HostClient): PluginLogger {
  const emit = (level: LogLevelDto, message: string, fields?: unknown) =>
    host.logEmit({ level, message, fields });
  return {
    debug: (message, fields) => emit("debug", message, fields),
    info: (message, fields) => emit("info", message, fields),
    warn: (message, fields) => emit("warn", message, fields),
    error: (message, fields) => emit("error", message, fields),
  };
}

function createStorage(host: HostClient): PluginStorage {
  return {
    async get(key) {
      const { value } = await host.storageGet({ key });
      return value;
    },
    async set(key, value) {
      await host.storageSet({ key, value });
    },
    async delete(key) {
      const { existed } = await host.storageDelete({ key });
      return existed;
    },
    async list(prefix) {
      const { keys } = await host.storageList({ prefix: prefix ?? null });
      return keys;
    },
  };
}

/** Slack added to the transport timeout so a server-side wait/long-poll
 * deadline always fires before the RPC's own timeout. */
const AGENT_RPC_TIMEOUT_SLACK_MS = 5_000;
/** Mirrors the host's `agent_wait` default budget. */
const AGENT_WAIT_DEFAULT_TIMEOUT_MS = 30_000;

function createAgents(host: HostClient): PluginAgents {
  return {
    async spawn(spec) {
      const { id } = await host.agentSpawn(spec);
      return id;
    },
    async wait(id, timeoutMs) {
      const budget = timeoutMs ?? AGENT_WAIT_DEFAULT_TIMEOUT_MS;
      return host.agentWait(
        { id, timeout_ms: budget },
        { timeoutMs: budget + AGENT_RPC_TIMEOUT_SLACK_MS },
      );
    },
    async events(id, cursor, timeoutMs) {
      const budget = timeoutMs ?? 0;
      return host.agentEvents(
        { id, cursor: cursor ?? 0, timeout_ms: budget },
        { timeoutMs: budget + AGENT_RPC_TIMEOUT_SLACK_MS },
      );
    },
    async list() {
      const { agents } = await host.agentList();
      return agents;
    },
    async cancel(id) {
      const { outcome } = await host.agentCancel({ id });
      return outcome;
    },
  };
}

/** Builds the `PluginContext` handed to `setup()` and every hook. */
export function createPluginContext(
  host: HostClient,
  init: InitializeParams,
): PluginContext {
  return {
    workspaceRoot: init.workspace_root,
    sessionId: init.session_id,
    log: createLogger(host),
    storage: createStorage(host),
    agents: createAgents(host),
    async config<T = unknown>(): Promise<T> {
      const { value } = await host.configGet();
      return value as T;
    },
  };
}
