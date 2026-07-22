// demo-hooks — a minimal TypeScript sidecar plugin for grok-build.
//
// Demonstrates the hook shapes a plugin can drive through the sidecar
// wire contract:
//
//   • pre_tool_use      — a *Tool* gate: deny a tool call whose input carries a
//                         marker string (parity target for the e2e test).
//   • stop              — a *Stop* gate: inject `additionalContext` into the
//                         next turn without blocking the stop.
//   • session_start     — an *Observe* event: log that the session began.
//   • resolve_credential — a *Replace* gate: hand the core a fixed bearer
//                         instead of its built-in credential resolution.
//
// …plus one model-visible tool (`echo`), declared in plugin.json's `tools`
// array (the catalog's source of truth) and served here via `tool_invoke`.
// The model calls it as `demo-hooks__echo`; the handler runs in this sidecar
// with the full ctx (storage/agents/log/config) and a per-call context —
// note how it reads `call.cwd`, which is the *calling* session's working
// directory at invoke time, not a session-static path.
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
  replace,
  type HookInvokeResult,
  type PluginCredentialDto,
} from "../../../sdk/plugin/src/index.ts";

/** Tool inputs containing this marker are denied by `pre_tool_use`. */
export const DENY_MARKER = "DEMO_DENY_MARKER";

/** The exact deny reason surfaced to the model (the e2e test asserts parity). */
export const DENY_REASON =
  "demo-hooks denied: tool input contained the demo marker";

/** Context injected on `stop` (the e2e test asserts parity on this string). */
export const STOP_CONTEXT =
  "demo-hooks: remember to run the demo checklist before stopping";

/** A fixed bearer this demo hands the core via `resolve_credential`. Not a real
 * provider token — it just shows the Replace credential shape end to end. */
export const STATIC_TOKEN = "demo-static-bearer-0123456789";

definePlugin({
  name: "demo-hooks",
  tools: {
    // Keep the descriptor fields in sync with plugin.json's `tools` entry —
    // the host warns at handshake when the two drift.
    echo: {
      description: "Echo the given text back, with the caller's context.",
      inputSchema: {
        type: "object",
        properties: { text: { type: "string", description: "Text to echo" } },
        required: ["text"],
      },
      handler(input, ctx, call) {
        const text = (input as { text?: unknown }).text ?? "";
        ctx.log.info("demo-hooks: echo tool called", { cwd: call.cwd });
        return `demo-echo: ${String(text)} (cwd=${call.cwd}, agent=${call.agent})`;
      },
    },
  },
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

    // Replace gate (credential seam): supply a fixed bearer instead of the
    // core's built-in resolution. A real plugin would fetch this from an
    // external identity provider; here it is a static token so the wire shape
    // is exercised without any real credentials. Return the credential via
    // `replace(...)`; returning `observed()` (or nothing) passes through and
    // the core keeps its built-in resolution.
    resolve_credential(): HookInvokeResult {
      const credential: PluginCredentialDto = {
        token: STATIC_TOKEN,
        needs_token_auth_header: true,
        expires_at_ms: null,
        owner_id: "demo-hooks",
      };
      return replace(credential);
    },
  },
});
