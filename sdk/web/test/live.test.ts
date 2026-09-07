// Drives the real `App` against a running `grok agent gateway`.
//
// Skipped unless `GROK_WEB_LIVE_URL` and `GROK_WEB_LIVE_SECRET` are set, because
// it needs a leader, a model provider and the `panel-probe` fixture installed —
// see the README. When it does run it is the only test here that proves the
// seam rather than the client's own arithmetic: same WebSocket, same ACP
// frames, same DOM the browser builds.
import { describe, expect, test } from "bun:test";

import { App } from "../src/app.ts";

const URL_ = process.env["GROK_WEB_LIVE_URL"];
const SECRET = process.env["GROK_WEB_LIVE_SECRET"];
const live = URL_ && SECRET ? describe : describe.skip;

function settle(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function until(
  predicate: () => boolean,
  ms = 20_000,
  why: () => string = () => "",
): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (predicate()) return;
    // Stand in for the user at a shared permission modal. Waiting instead would
    // hang exactly the way an unanswered prompt hangs the real session: the
    // agent has no timeout on that round-trip.
    const option = document.querySelector(".permission-options button");
    if (option) (option as HTMLButtonElement).click();
    await settle(100);
  }
  throw new Error(`timed out waiting for the gateway: ${why()}`);
}

live("against a live gateway", () => {
  test("connects, groups the roster by cwd, attaches and renders a plugin panel", async () => {
    document.body.innerHTML = "";
    localStorage.clear();
    const root = document.createElement("div");
    document.body.append(root);
    new App(root).mount();

    const url = root.querySelector(".connect-url") as HTMLInputElement;
    const secret = root.querySelector(".connect-secret") as HTMLInputElement;
    url.value = URL_!;
    secret.value = SECRET!;
    (root.querySelector(".connect-button") as HTMLButtonElement).click();

    const status = () => root.querySelector(".status")?.textContent ?? "";
    await until(() => status() === "connected");

    // The roster arrived and is grouped by working directory.
    const groups = root.querySelectorAll(".roster-group");
    expect(groups.length).toBeGreaterThan(0);
    const paths = [...root.querySelectorAll(".roster-cwd-full")].map((n) => n.textContent ?? "");
    expect(new Set(paths).size).toBe(paths.length);

    // The settings catalog came over the wire and the terminal rows were
    // skipped because the wire said so.
    expect(root.querySelector(".settings summary")?.textContent).toMatch(
      /Settings — \d+ shown, \d+ terminal-only/,
    );

    // Attach to the first resident session.
    const rows = [...root.querySelectorAll(".roster-row")] as HTMLButtonElement[];
    expect(rows.length).toBeGreaterThan(0);
    rows[0]!.click();
    await until(() => status().startsWith("attached to"));

    // The `panel-probe` fixture publishes on `session_start`, and the panel is
    // replayed to a client that attaches later.
    await until(() => root.querySelectorAll(".grok-panel").length > 0);
    const panel = root.querySelector(".grok-panel") as HTMLElement;
    expect(panel.querySelector(".grok-panel-source")?.textContent).toBe("panel-probe");
    expect(panel.querySelectorAll(".grok-panel-chip").length).toBeGreaterThan(0);
    expect(panel.querySelector(".grok-md-h2")).not.toBeNull();
    expect(panel.querySelectorAll(".grok-panel-table tbody tr").length).toBeGreaterThan(0);
    expect(panel.querySelectorAll(".grok-panel-input input")).toHaveLength(2);
    expect(panel.querySelectorAll(".grok-panel-button")).toHaveLength(2);

    // A button press routes back to the plugin, which republishes the panel
    // carrying what it received.
    const note = panel.querySelector(".grok-panel-input input") as HTMLInputElement;
    note.value = "from the live test";
    (panel.querySelector(".grok-panel-button") as HTMLButtonElement).click();
    await until(() =>
      (root.querySelector(".grok-panel")?.textContent ?? "").includes("from the live test"),
    );
  }, 60_000);

  test("sends a prompt and streams the reply back", async () => {
    document.body.innerHTML = "";
    localStorage.clear();
    const root = document.createElement("div");
    document.body.append(root);
    new App(root).mount();

    (root.querySelector(".connect-url") as HTMLInputElement).value = URL_!;
    (root.querySelector(".connect-secret") as HTMLInputElement).value = SECRET!;
    (root.querySelector(".connect-button") as HTMLButtonElement).click();

    const status = () => root.querySelector(".status")?.textContent ?? "";
    await until(() => status() === "connected");

    // A brand-new session, not whichever row is first. Attaching to an existing
    // one queues this prompt behind any turn still running there — the leader
    // keeps a session resident across a client disconnect, so a turn outlives
    // the client that started it and the next prompt waits its turn.
    (root.querySelector(".roster-new") as HTMLButtonElement).click();
    await until(() => status().startsWith("attached to"), 60_000, status);

    const input = root.querySelector(".prompt-input") as HTMLTextAreaElement;
    input.value = "Reply with exactly: PONG. Nothing else.";
    (root.querySelector(".send") as HTMLButtonElement).click();

    await until(() => status().startsWith("turn ended"), 120_000, status);
    const assistant = [...root.querySelectorAll(".message-assistant .message-text")].map(
      (n) => n.textContent ?? "",
    );
    expect(assistant.join("")).toContain("PONG");
  }, 180_000);
});
