// Node ESM + type-stripping smoke check. Not part of the `bun test` suite:
// run directly with `node --experimental-strip-types test/smoke.node.ts`
// (see README). Verifies the two things a real Node run needs that `bun
// test`/`tsc` alone don't prove: that every relative import resolves as a
// real ESM specifier (explicit `.ts` extensions) and that the module graph
// is erasable-syntax-only (no enums/namespaces/etc. Node's stripper can't
// erase). Deliberately does NOT call `definePlugin()` — that starts a real
// stdio read loop against this process's actual stdin and calls
// `process.exit()` on shutdown, neither of which belongs in a smoke check.

import * as sdk from "../src/index.ts";

const requiredExports = [
  "definePlugin",
  "allow",
  "deny",
  "stopBlock",
  "forceStop",
  "observed",
  "replace",
  "PROTOCOL_VERSION",
  "JsonRpcEndpoint",
  "HostClient",
  "createPluginContext",
  "registerIncomingHandlers",
  "defaultByteReader",
  "defaultByteWriter",
] as const;

let ok = true;
for (const name of requiredExports) {
  if (!(name in sdk)) {
    ok = false;
    console.error(`FAIL: missing export "${name}"`);
  }
}

if (sdk.PROTOCOL_VERSION !== 1) {
  ok = false;
  console.error(`FAIL: PROTOCOL_VERSION expected 1, got ${sdk.PROTOCOL_VERSION}`);
}

// Exercise a bit of real logic without touching stdio: the gate-aware
// result helpers should build the exact wire shapes.
const decision = sdk.allow("fine");
if (decision.kind !== "decision" || decision.decision !== "allow") {
  ok = false;
  console.error("FAIL: allow() did not build the expected wire shape");
}

if (!ok) {
  console.error("Node smoke check FAILED");
  process.exit(1);
}

console.log("Node smoke check OK: module graph resolves and loads under --experimental-strip-types");
