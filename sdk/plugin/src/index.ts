// Public API of @grok-build/plugin. Re-exports the generated wire types
// (do not redefine any of these shapes by hand — see src/generated/*.ts)
// plus the SDK's runtime: stdio transport, typed RPC layer, plugin context,
// and `definePlugin`.

// --- Generated wire types (source of truth: xai-grok-plugin-protocol via ts-rs) ---
export type { AgentCancelOutcomeDto } from "./generated/AgentCancelOutcomeDto.ts";
export type { AgentCancelParams } from "./generated/AgentCancelParams.ts";
export type { AgentCancelResult } from "./generated/AgentCancelResult.ts";
export type { AgentEventDto } from "./generated/AgentEventDto.ts";
export type { AgentEventKindDto } from "./generated/AgentEventKindDto.ts";
export type { AgentEventsParams } from "./generated/AgentEventsParams.ts";
export type { AgentEventsResult } from "./generated/AgentEventsResult.ts";
export type { AgentListParams } from "./generated/AgentListParams.ts";
export type { AgentListResult } from "./generated/AgentListResult.ts";
export type { AgentSpawnParams } from "./generated/AgentSpawnParams.ts";
export type { AgentSpawnResult } from "./generated/AgentSpawnResult.ts";
export type { AgentStatusDto } from "./generated/AgentStatusDto.ts";
export type { AgentWaitParams } from "./generated/AgentWaitParams.ts";
export type { AgentWaitResult } from "./generated/AgentWaitResult.ts";
export type { ConfigGetParams } from "./generated/ConfigGetParams.ts";
export type { ConfigGetResult } from "./generated/ConfigGetResult.ts";
export type { DecisionDto } from "./generated/DecisionDto.ts";
export type { EventName } from "./generated/EventName.ts";
export type { GateKindDto } from "./generated/GateKindDto.ts";
export type { HookInvokeParams } from "./generated/HookInvokeParams.ts";
export type { HookInvokeResult } from "./generated/HookInvokeResult.ts";
export type { HostCapabilities } from "./generated/HostCapabilities.ts";
export type { InitializeParams } from "./generated/InitializeParams.ts";
export type { InitializeResult } from "./generated/InitializeResult.ts";
export type { LogEmitParams } from "./generated/LogEmitParams.ts";
export type { LogLevelDto } from "./generated/LogLevelDto.ts";
// Per-event hook payloads + their nested DTOs (see `HookPayloadMap`).
export type { SubagentStopPhaseDto } from "./generated/SubagentStopPhaseDto.ts";
export type { BackgroundTaskTypeDto } from "./generated/BackgroundTaskTypeDto.ts";
export type { StopFailureKindDto } from "./generated/StopFailureKindDto.ts";
export type { StopBackgroundTaskDto } from "./generated/StopBackgroundTaskDto.ts";
export type { StopSessionCronDto } from "./generated/StopSessionCronDto.ts";
export type { ProviderResponseToolCallDto } from "./generated/ProviderResponseToolCallDto.ts";
export type { SessionStartPayload } from "./generated/SessionStartPayload.ts";
export type { SessionEndPayload } from "./generated/SessionEndPayload.ts";
export type { StopPayload } from "./generated/StopPayload.ts";
export type { StopFailurePayload } from "./generated/StopFailurePayload.ts";
export type { PreToolUsePayload } from "./generated/PreToolUsePayload.ts";
export type { PostToolUsePayload } from "./generated/PostToolUsePayload.ts";
export type { PostToolUseFailurePayload } from "./generated/PostToolUseFailurePayload.ts";
export type { PermissionDeniedPayload } from "./generated/PermissionDeniedPayload.ts";
export type { UserPromptSubmitPayload } from "./generated/UserPromptSubmitPayload.ts";
export type { NotificationPayload } from "./generated/NotificationPayload.ts";
export type { SubagentStartPayload } from "./generated/SubagentStartPayload.ts";
export type { SubagentStopPayload } from "./generated/SubagentStopPayload.ts";
export type { PreCompactPayload } from "./generated/PreCompactPayload.ts";
export type { PostCompactPayload } from "./generated/PostCompactPayload.ts";
export type { ProviderRequestPayload } from "./generated/ProviderRequestPayload.ts";
export type { ProviderResponsePayload } from "./generated/ProviderResponsePayload.ts";
export type { ProviderErrorPayload } from "./generated/ProviderErrorPayload.ts";
export type { SubagentResolvePayload } from "./generated/SubagentResolvePayload.ts";
export type { ResolveCredentialPayload } from "./generated/ResolveCredentialPayload.ts";
export type { RefreshCredentialPayload } from "./generated/RefreshCredentialPayload.ts";
export type { StartOauthFlowPayload } from "./generated/StartOauthFlowPayload.ts";
export type { PanelActionParams } from "./generated/PanelActionParams.ts";
export type { PanelBlock } from "./generated/PanelBlock.ts";
export type { PanelButton } from "./generated/PanelButton.ts";
export type { PanelCloseParams } from "./generated/PanelCloseParams.ts";
export type { PanelCloseResult } from "./generated/PanelCloseResult.ts";
export type { PanelPublishResult } from "./generated/PanelPublishResult.ts";
export type { PanelStatusItem } from "./generated/PanelStatusItem.ts";
export type { PanelTone } from "./generated/PanelTone.ts";
export type { PanelViewModel } from "./generated/PanelViewModel.ts";
export type { PluginCredentialDto } from "./generated/PluginCredentialDto.ts";
export type { ShutdownParams } from "./generated/ShutdownParams.ts";
export type { StorageDeleteParams } from "./generated/StorageDeleteParams.ts";
export type { StorageDeleteResult } from "./generated/StorageDeleteResult.ts";
export type { StorageGetParams } from "./generated/StorageGetParams.ts";
export type { StorageGetResult } from "./generated/StorageGetResult.ts";
export type { StorageListParams } from "./generated/StorageListParams.ts";
export type { StorageListResult } from "./generated/StorageListResult.ts";
export type { StorageSetParams } from "./generated/StorageSetParams.ts";
export type { StorageSetResult } from "./generated/StorageSetResult.ts";
export type { ToolCallContextDto } from "./generated/ToolCallContextDto.ts";
export type { ToolDescriptorDto } from "./generated/ToolDescriptorDto.ts";
export type { ToolInvokeParams } from "./generated/ToolInvokeParams.ts";
export type { ToolInvokeResult } from "./generated/ToolInvokeResult.ts";

// --- Transport ---
export {
  JsonRpcEndpoint,
  LineReader,
  LineWriter,
  encodeLine,
  defaultByteReader,
  defaultByteWriter,
  JsonRpcErrorCode,
  JsonRpcRemoteError,
  RpcTimeoutError,
  MAX_LINE_BYTES,
} from "./stdio.ts";
export type {
  ByteReader,
  ByteWriter,
  JsonRpcId,
  JsonRpcRequest,
  JsonRpcNotification,
  JsonRpcSuccess,
  JsonRpcErrorResponse,
  JsonRpcErrorObject,
  JsonRpcEndpointOptions,
  RequestHandler,
  NotificationHandler,
} from "./stdio.ts";

// --- Typed RPC layer ---
export {
  HostClient,
  registerIncomingHandlers,
  CoreToPluginMethod,
  PluginToCoreMethod,
} from "./rpc.ts";
export type { IncomingHandlers } from "./rpc.ts";

// --- Plugin context ---
export { createPluginContext } from "./context.ts";
export type {
  PluginAgents,
  PluginContext,
  PluginLogger,
  PluginStorage,
  PluginUi,
  ToolCallContext,
} from "./context.ts";

// --- definePlugin + gate-aware result helpers ---
export {
  definePlugin,
  allow,
  deny,
  stopBlock,
  forceStop,
  observed,
  replace,
  PROTOCOL_VERSION,
} from "./define.ts";
export type {
  PluginDefinition,
  DefinePluginOptions,
  PluginHandle,
  HookHandler,
  HookPayloadMap,
  HookResult,
  Teardown,
  ToolDefinition,
  ToolHandler,
  ToolResult,
} from "./define.ts";
