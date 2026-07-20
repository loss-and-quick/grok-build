// `PluginContext`: the object every hook and `setup()` receives. Thin,
// typed wrapper over `HostClient` plus the static bits handed over at
// `initialize` time (`workspaceRoot`, `sessionId`).

import type { HostClient } from "./rpc.ts";
import type { InitializeParams } from "./generated/InitializeParams.ts";
import type { LogLevelDto } from "./generated/LogLevelDto.ts";

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

export interface PluginContext {
  readonly workspaceRoot: string;
  readonly sessionId: string;
  readonly log: PluginLogger;
  readonly storage: PluginStorage;
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
    async config<T = unknown>(): Promise<T> {
      const { value } = await host.configGet();
      return value as T;
    },
  };
}
