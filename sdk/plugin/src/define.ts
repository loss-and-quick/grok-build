// `definePlugin`: the entire program for a TS plugin. Calling it wires up
// the stdio JSON-RPC endpoint, serves `initialize`/`hook_invoke`/`shutdown`,
// and (in production) never returns control to the caller in any meaningful
// sense — the process lives for as long as the host keeps the sidecar open.

import process from "node:process";

import {
  JsonRpcEndpoint,
  type ByteReader,
  type ByteWriter,
} from "./stdio.ts";
import { registerIncomingHandlers, HostClient } from "./rpc.ts";
import { createPluginContext, type PluginContext } from "./context.ts";

import type { EventName } from "./generated/EventName.ts";
import type { HookInvokeResult } from "./generated/HookInvokeResult.ts";
import type { InitializeResult } from "./generated/InitializeResult.ts";

/** The plugin SDK's own protocol version (wire contract v1). */
export const PROTOCOL_VERSION = 1;

/**
 * The `hook_invoke` reply shape. Parameterized by event name for API
 * symmetry with the wire dictionary; v1 does not give each `EventName` a
 * distinct statically-known gate (that mapping lives in
 * `xai-grok-hooks::event` on the Rust side), so every event accepts the same
 * `HookInvokeResult` union. Use the `allow`/`deny`/`stopBlock`/`forceStop`/
 * `observed`/`replace` helpers to build a value valid for the event's actual
 * gate; the host fails open if a plugin returns a shape its gate doesn't
 * expect.
 */
export type HookResult<_E extends EventName = EventName> = HookInvokeResult;

/** A hook handler. Returning `undefined`/`void` means passthrough (`observed`). */
export type HookHandler<E extends EventName = EventName> = (
  payload: unknown,
  ctx: PluginContext,
) => Promise<HookResult<E> | void> | HookResult<E> | void;

/** Optional cleanup returned by `setup()`, run (best-effort) on `shutdown`. */
export type Teardown = () => Promise<void> | void;

export interface PluginDefinition {
  name?: string;
  hooks?: { [E in EventName]?: HookHandler<E> };
  setup?: (ctx: PluginContext) => Promise<void | Teardown> | void | Teardown;
}

export interface DefinePluginOptions {
  /** Injectable transport, for tests. Defaults to the real process stdin/stdout. */
  reader?: ByteReader;
  writer?: ByteWriter;
  /** Skip the self-`process.exit()` on shutdown (tests drive the process lifecycle themselves). */
  exitOnShutdown?: boolean;
}

export interface PluginHandle {
  readonly endpoint: JsonRpcEndpoint;
  /** Resolves once `initialize` has been handled and `setup()` has run. */
  readonly whenReady: Promise<void>;
  /** Resolves once `shutdown` has been handled (teardown run, best effort). */
  readonly whenShutdown: Promise<void>;
}

const SHUTDOWN_GRACE_MS = 1_800;

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** `{ "kind": "observed" }` — passthrough, no opinion. */
export function observed(): HookInvokeResult {
  return { kind: "observed" };
}

/** Tool gate: allow the action. */
export function allow(reason?: string): HookInvokeResult {
  return { kind: "decision", decision: "allow", reason };
}

/** Tool gate: deny the action. */
export function deny(reason: string): HookInvokeResult {
  return { kind: "decision", decision: "deny", reason };
}

/**
 * Stop gate: block this stop/step and keep the session going, e.g. to force
 * the agent to keep working with `reason` (and optional `additionalContext`)
 * fed back in. Does not abort the overall run.
 */
export function stopBlock(
  reason: string,
  additionalContext?: string,
): HookInvokeResult {
  return {
    kind: "stop",
    block: true,
    reason,
    continue: true,
    additional_context: additionalContext,
  };
}

/**
 * Stop gate: hard-abort the overall run (`continue: false`), e.g. a fatal
 * condition the plugin detected that the host should not proceed past.
 */
export function forceStop(reason: string): HookInvokeResult {
  return {
    kind: "stop",
    block: true,
    reason,
    continue: false,
  };
}

/** Replace gate: substitute `payload` (omit/`undefined` = passthrough). */
export function replace(payload?: unknown): HookInvokeResult {
  return { kind: "replace", payload };
}

/**
 * Defines and starts a TS plugin: wires the stdio JSON-RPC endpoint, serves
 * `initialize`/`hook_invoke`/`shutdown`, and starts the read loop
 * immediately. In a real plugin entry point this call *is* the whole
 * program — nothing after it needs to run.
 */
export function definePlugin(
  def: PluginDefinition,
  options: DefinePluginOptions = {},
): PluginHandle {
  const endpoint = new JsonRpcEndpoint({
    reader: options.reader,
    writer: options.writer,
  });
  const host = new HostClient(endpoint);
  const subscriptions = Object.keys(def.hooks ?? {}) as EventName[];

  let ctx: PluginContext | undefined;
  let teardown: Teardown | undefined;
  let resolveReady!: () => void;
  let resolveShutdown!: () => void;
  const whenReady = new Promise<void>((resolve) => {
    resolveReady = resolve;
  });
  const whenShutdown = new Promise<void>((resolve) => {
    resolveShutdown = resolve;
  });

  registerIncomingHandlers(endpoint, {
    async initialize(params): Promise<InitializeResult> {
      ctx = createPluginContext(host, params);
      try {
        const maybeTeardown = await def.setup?.(ctx);
        if (typeof maybeTeardown === "function") teardown = maybeTeardown;
      } finally {
        resolveReady();
      }
      return {
        protocol_version: PROTOCOL_VERSION,
        subscriptions,
        plugin_version: undefined,
      };
    },

    async hookInvoke(params): Promise<HookInvokeResult> {
      const handler = def.hooks?.[params.event as EventName];
      if (!handler || !ctx) return observed();
      try {
        const result = await handler(params.payload, ctx);
        return result ?? observed();
      } catch (err) {
        host.logEmit({
          level: "error",
          message: `hook "${params.event}" threw`,
          fields: { error: err instanceof Error ? err.message : String(err) },
        });
        // Fail-open: an uncaught hook error must never block the host.
        return observed();
      }
    },

    async shutdown(): Promise<void> {
      try {
        await Promise.race([
          (async () => {
            if (teardown) await teardown();
          })(),
          delay(SHUTDOWN_GRACE_MS),
        ]);
      } catch (err) {
        host.logEmit({
          level: "error",
          message: "teardown threw during shutdown",
          fields: { error: err instanceof Error ? err.message : String(err) },
        });
      } finally {
        resolveShutdown();
        if (options.exitOnShutdown ?? true) {
          process.exit(0);
        }
      }
    },
  });

  endpoint.start();

  return { endpoint, whenReady, whenShutdown };
}
