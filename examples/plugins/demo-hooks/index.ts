// demo-hooks — a minimal TypeScript sidecar plugin for grok-build.
//
// Demonstrates the three hook shapes a plugin can drive through the sidecar
// wire contract:
//
//   • pre_tool_use  — a *Tool* gate: deny a tool call whose input carries a
//                     marker string (parity target for the e2e test).
//   • stop          — a *Stop* gate: inject `additionalContext` into the next
//                     turn without blocking the stop.
//   • session_start — an *Observe* event: log that the session began.
//
// Import note: a real, installed plugin imports the SDK by its package name:
//
//     import { definePlugin, deny, observed } from "@grok-build/plugin";
//
// This in-repo example has no `node_modules`, so it imports the SDK source
// directly by relative path. The runtime (bun / node >=22 / deno) executes the
// TypeScript entry file as-is — no build step.
import {
  definePlugin,
  deny,
  observed,
  type HookInvokeResult,
} from "../../../sdk/plugin/src/index.ts";

/** Tool inputs containing this marker are denied by `pre_tool_use`. */
export const DENY_MARKER = "DEMO_DENY_MARKER";

/** The exact deny reason surfaced to the model (the e2e test asserts parity). */
export const DENY_REASON =
  "demo-hooks denied: tool input contained the demo marker";

/** Context injected on `stop` (the e2e test asserts parity on this string). */
export const STOP_CONTEXT =
  "demo-hooks: remember to run the demo checklist before stopping";

definePlugin({
  name: "demo-hooks",
  hooks: {
    // Observe-only: the return value is ignored, but we log the session id so
    // the host's capability channel (`log_emit`) is exercised end to end.
    session_start(_payload, ctx): HookInvokeResult {
      ctx.log.info("demo-hooks: session started", { sessionId: ctx.sessionId });
      return observed();
    },

    // Tool gate: deny when the tool input carries the demo marker anywhere in
    // its JSON. Everything else passes through untouched.
    pre_tool_use(payload): HookInvokeResult {
      const input = (payload as { toolInput?: unknown }).toolInput ?? {};
      if (JSON.stringify(input).includes(DENY_MARKER)) {
        return deny(DENY_REASON);
      }
      return observed();
    },

    // Stop gate: inject additional context for the next turn without blocking
    // the stop (`block: false`) — the agent still stops, but the model sees the
    // reminder if the run continues.
    stop(): HookInvokeResult {
      return {
        kind: "stop",
        block: false,
        additional_context: STOP_CONTEXT,
      };
    },
  },
});
