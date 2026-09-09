import { describe, expect, test } from "bun:test";

import { TICKS_PER_SECOND } from "../src/animation.ts";
import { BLINK_TICKS, composeTitle, shouldInterrupt } from "../src/notify.ts";

const facts = (over: Partial<Parameters<typeof composeTitle>[0]> = {}) =>
  composeTitle({
    waiting: 0,
    busy: false,
    sessionName: null,
    focused: true,
    blinkOn: true,
    ...over,
  });

describe("the tab title says what the terminal's window title says", () => {
  test("with nothing happening it is just the product name", () => {
    expect(facts()).toBe("grok");
  });

  test("a blocked turn comes first, ahead of the session's own name", () => {
    // `TitleConfig::default` lists `ActionRequired` before `SessionName`: what
    // is stopped outranks what it is called.
    expect(facts({ waiting: 1, sessionName: "refactor the parser" })).toBe(
      "⚠ Action Required - refactor the parser - grok",
    );
  });

  test("more than one waiting is counted, because how many is the question", () => {
    expect(facts({ waiting: 3 })).toBe("⚠ Action Required (3) - grok");
  });

  test("a long session name is cut to the terminal's own limit", () => {
    const long = "a".repeat(60);
    const title = facts({ sessionName: long });
    expect(title).toBe(`${"a".repeat(39)}… - grok`);
  });

  test("a running turn says so, and stops saying so when it ends", () => {
    expect(facts({ busy: true, sessionName: "one" })).toBe("Working - one - grok");
    expect(facts({ busy: false, sessionName: "one" })).toBe("one - grok");
  });
});

describe("the flag blinks for attention and stands still for a reader", () => {
  test("the off half of the cycle drops it, so the title oscillates", () => {
    expect(facts({ waiting: 1, focused: false, blinkOn: true })).toContain("Action Required");
    expect(facts({ waiting: 1, focused: false, blinkOn: false })).not.toContain("Action Required");
  });

  test("a focused tab holds it steady, whatever the cycle says", () => {
    // The pager's reason, transcribed: oscillation while someone is actually
    // interacting with the card is distraction, not attention.
    expect(facts({ waiting: 1, focused: true, blinkOn: false })).toContain("Action Required");
  });

  test("the cadence is the artifact's, not a number typed here", () => {
    // 15 ticks at the pager's 30 a second — one second a cycle. A retune of the
    // artifact moves both clients rather than leaving this one behind.
    expect(BLINK_TICKS).toBe(TICKS_PER_SECOND / 2);
  });
});

describe("what is worth interrupting for", () => {
  test("only when the tab is not the one being looked at", () => {
    // `NotificationCondition::Unfocused` is the pager's default. Someone
    // watching the turn does not need to be told about it.
    expect(shouldInterrupt(false)).toBe(true);
    expect(shouldInterrupt(true)).toBe(false);
  });
});
