// Typed core↔plugin RPC layer over `JsonRpcEndpoint`. Every method name and
// DTO shape here comes straight from the wire contract / generated ts-rs
// types — this module only adds routing and a typed call surface, it never
// invents wire shapes.

import type { JsonRpcEndpoint } from "./stdio.ts";

import type { InitializeParams } from "./generated/InitializeParams.ts";
import type { InitializeResult } from "./generated/InitializeResult.ts";
import type { HookInvokeParams } from "./generated/HookInvokeParams.ts";
import type { HookInvokeResult } from "./generated/HookInvokeResult.ts";
import type { ShutdownParams } from "./generated/ShutdownParams.ts";
import type { ToolInvokeParams } from "./generated/ToolInvokeParams.ts";
import type { ToolInvokeResult } from "./generated/ToolInvokeResult.ts";
import type { ToolCancelParams } from "./generated/ToolCancelParams.ts";
import type { LogEmitParams } from "./generated/LogEmitParams.ts";
import type { ConfigGetResult } from "./generated/ConfigGetResult.ts";
import type { StorageGetParams } from "./generated/StorageGetParams.ts";
import type { StorageGetResult } from "./generated/StorageGetResult.ts";
import type { StorageSetParams } from "./generated/StorageSetParams.ts";
import type { StorageSetResult } from "./generated/StorageSetResult.ts";
import type { StorageDeleteParams } from "./generated/StorageDeleteParams.ts";
import type { StorageDeleteResult } from "./generated/StorageDeleteResult.ts";
import type { StorageListParams } from "./generated/StorageListParams.ts";
import type { StorageListResult } from "./generated/StorageListResult.ts";
import type { AgentSpawnParams } from "./generated/AgentSpawnParams.ts";
import type { AgentSpawnResult } from "./generated/AgentSpawnResult.ts";
import type { AgentWaitParams } from "./generated/AgentWaitParams.ts";
import type { AgentWaitResult } from "./generated/AgentWaitResult.ts";
import type { AgentEventsParams } from "./generated/AgentEventsParams.ts";
import type { AgentEventsResult } from "./generated/AgentEventsResult.ts";
import type { AgentListResult } from "./generated/AgentListResult.ts";
import type { AgentCancelParams } from "./generated/AgentCancelParams.ts";
import type { AgentCancelResult } from "./generated/AgentCancelResult.ts";

/** Core→plugin method names, v1 (see wire-contract-v1.md). */
export const CoreToPluginMethod = {
  Initialize: "initialize",
  HookInvoke: "hook_invoke",
  ToolInvoke: "tool_invoke",
  ToolCancel: "tool_cancel",
  Shutdown: "shutdown",
} as const;

/** Plugin→core method names, v1 (see wire-contract-v1.md). */
export const PluginToCoreMethod = {
  LogEmit: "log_emit",
  StorageGet: "storage_get",
  StorageSet: "storage_set",
  StorageDelete: "storage_delete",
  StorageList: "storage_list",
  ConfigGet: "config_get",
  AgentSpawn: "agent_spawn",
  AgentWait: "agent_wait",
  AgentEvents: "agent_events",
  AgentList: "agent_list",
  AgentCancel: "agent_cancel",
} as const;

/** Handlers for the core→plugin methods a plugin must serve. */
export interface IncomingHandlers {
  initialize(
    params: InitializeParams,
  ): Promise<InitializeResult> | InitializeResult;
  hookInvoke(
    params: HookInvokeParams,
  ): Promise<HookInvokeResult> | HookInvokeResult;
  toolInvoke(
    params: ToolInvokeParams,
  ): Promise<ToolInvokeResult> | ToolInvokeResult;
  /** Notification: the host abandoned an in-flight `tool_invoke` (parent turn
   * aborted). No reply; the SDK aborts the matching handler's signal. */
  toolCancel(params: ToolCancelParams): void;
  shutdown(params: ShutdownParams): Promise<void> | void;
}

/** Wires `handlers` up as the endpoint's request/notification handlers. */
export function registerIncomingHandlers(
  endpoint: JsonRpcEndpoint,
  handlers: IncomingHandlers,
): void {
  endpoint.setRequestHandler(CoreToPluginMethod.Initialize, (params) =>
    handlers.initialize(params as InitializeParams),
  );
  endpoint.setRequestHandler(CoreToPluginMethod.HookInvoke, (params) =>
    handlers.hookInvoke(params as HookInvokeParams),
  );
  endpoint.setRequestHandler(CoreToPluginMethod.ToolInvoke, (params) =>
    handlers.toolInvoke(params as ToolInvokeParams),
  );
  endpoint.setNotificationHandler(CoreToPluginMethod.ToolCancel, (params) =>
    handlers.toolCancel(params as ToolCancelParams),
  );
  endpoint.setNotificationHandler(CoreToPluginMethod.Shutdown, (params) =>
    handlers.shutdown(params as ShutdownParams),
  );
}

/** Typed plugin→core calls, per the wire contract. */
export class HostClient {
  private readonly endpoint: JsonRpcEndpoint;

  constructor(endpoint: JsonRpcEndpoint) {
    this.endpoint = endpoint;
  }

  /** `log_emit` notification — fire and forget, no reply. */
  logEmit(params: LogEmitParams): void {
    void this.endpoint.notify(PluginToCoreMethod.LogEmit, params);
  }

  storageGet(params: StorageGetParams): Promise<StorageGetResult> {
    return this.endpoint.request<StorageGetResult>(
      PluginToCoreMethod.StorageGet,
      params,
    );
  }

  storageSet(params: StorageSetParams): Promise<StorageSetResult> {
    return this.endpoint.request<StorageSetResult>(
      PluginToCoreMethod.StorageSet,
      params,
    );
  }

  storageDelete(params: StorageDeleteParams): Promise<StorageDeleteResult> {
    return this.endpoint.request<StorageDeleteResult>(
      PluginToCoreMethod.StorageDelete,
      params,
    );
  }

  storageList(params: StorageListParams): Promise<StorageListResult> {
    return this.endpoint.request<StorageListResult>(
      PluginToCoreMethod.StorageList,
      params,
    );
  }

  configGet(): Promise<ConfigGetResult> {
    return this.endpoint.request<ConfigGetResult>(
      PluginToCoreMethod.ConfigGet,
      {},
    );
  }

  // --- Subagent orchestration (`agent_*`). The host answers
  // `method_not_found` when the session has no orchestration wiring;
  // feature-detect by catching that error on the first call. ---

  agentSpawn(params: AgentSpawnParams): Promise<AgentSpawnResult> {
    return this.endpoint.request<AgentSpawnResult>(
      PluginToCoreMethod.AgentSpawn,
      params,
    );
  }

  agentWait(
    params: AgentWaitParams,
    opts?: { timeoutMs?: number },
  ): Promise<AgentWaitResult> {
    return this.endpoint.request<AgentWaitResult>(
      PluginToCoreMethod.AgentWait,
      params,
      opts,
    );
  }

  agentEvents(
    params: AgentEventsParams,
    opts?: { timeoutMs?: number },
  ): Promise<AgentEventsResult> {
    return this.endpoint.request<AgentEventsResult>(
      PluginToCoreMethod.AgentEvents,
      params,
      opts,
    );
  }

  agentList(): Promise<AgentListResult> {
    return this.endpoint.request<AgentListResult>(
      PluginToCoreMethod.AgentList,
      {},
    );
  }

  agentCancel(params: AgentCancelParams): Promise<AgentCancelResult> {
    return this.endpoint.request<AgentCancelResult>(
      PluginToCoreMethod.AgentCancel,
      params,
    );
  }
}
