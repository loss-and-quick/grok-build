import { describe, expect, test } from "bun:test";

import {
  PERMISSION_MODES,
  SESSION_MODES,
  modeById,
  permissionModeOf,
  permissionModeParams,
  readModeUpdate,
} from "../src/modes.ts";

describe("only the modes the agent was seen to enter are offered", () => {
  test("the two that broadcast a confirmation, and no others", () => {
    // Probed live: `plan` and `default` are answered *and* broadcast back as
    // `current_mode_update`. `ask`, `acceptEdits`, `bypassPermissions` and a
    // nonsense string are answered `{}` with no broadcast at all — accepted and
    // ignored, which is the one outcome a control must never be built on.
    expect(SESSION_MODES.map((mode) => mode.id)).toEqual(["default", "plan"]);
    expect(modeById("acceptEdits")).toBeNull();
    expect(modeById("plan")?.label).toBe("Plan");
  });
});

describe("the mode on screen is the one the agent confirmed", () => {
  test("a `current_mode_update` is what carries it", () => {
    expect(readModeUpdate({ sessionUpdate: "current_mode_update", currentModeId: "plan" })).toBe(
      "plan",
    );
  });

  test("any other update carries nothing, so nothing moves", () => {
    expect(readModeUpdate({ sessionUpdate: "agent_message_chunk" })).toBeNull();
    expect(readModeUpdate({ sessionUpdate: "current_mode_update" })).toBeNull();
  });
});

describe("how much the agent decides alone is a different axis", () => {
  test("the three states are three combinations of the wire's own booleans", () => {
    expect(permissionModeParams("ask")).toEqual({
      yolo_mode: false,
      auto_mode: false,
      permission_mode: "ask",
    });
    // The one that matters most: with `auto_mode` on, a classifier answers every
    // permission request before any client is shown one.
    expect(permissionModeParams("auto")).toMatchObject({ yolo_mode: false, auto_mode: true });
    expect(permissionModeParams("always-approve")).toMatchObject({
      yolo_mode: true,
      permission_mode: "always-approve",
    });
  });

  test("no session id rides it, because the agent does not scope it that way", () => {
    // The pager sends none, and its own comment says the agent applies the
    // notification to every session of the sending client. A session id here
    // would be a scoping this client cannot actually ask for.
    expect(permissionModeParams("ask")).not.toHaveProperty("sessionId");
  });

  test("exactly one of them is confirmed first, and it is the one that stops the asking", () => {
    const confirmed = PERMISSION_MODES.filter((mode) => mode.confirm).map((mode) => mode.id);
    expect(confirmed).toEqual(["always-approve"]);
  });

  test("the roster can only prove one of the three, and does not guess the others", () => {
    // `RosterEntry` carries `yolo` and nothing else. "ask" and "auto" are
    // indistinguishable from there, and claiming "ask" would promise permission
    // requests that a session in auto mode will never send.
    expect(permissionModeOf(true)).toBe("always-approve");
    expect(permissionModeOf(false)).toBeNull();
    expect(permissionModeOf(undefined)).toBeNull();
  });
});
