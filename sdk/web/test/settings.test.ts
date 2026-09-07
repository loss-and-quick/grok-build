import { describe, expect, test } from "bun:test";

import catalog from "../../settings/src/generated/catalog.json" with { type: "json" };
import { visibleSettingRows, type SettingRow } from "../src/wire.ts";

const rows = catalog.rows as unknown as SettingRow[];

describe("settings surface", () => {
  test("the client filters on `surface`, and never on a list of keys", () => {
    const shown = visibleSettingRows(rows);
    expect(shown.length).toBeGreaterThan(0);
    for (const row of shown) expect(row.surface).not.toBe("terminal");
    for (const row of rows) {
      if (row.surface === "terminal") expect(shown).not.toContain(row);
    }
  });

  test("`surface` is only ever one of the two values the wire declares", () => {
    // A third value would mean the client is silently dropping rows it does not
    // recognise. Fail here instead.
    for (const row of rows) {
      expect(["any", "terminal"]).toContain(row.surface);
    }
  });

  test("the generated catalog still splits into terminal and non-terminal rows", () => {
    // Pinned counts, in the house style of `media_gen_limits`: a row that
    // changes surface is a deliberate decision about which clients draw it, so
    // it should be visible in a diff rather than discovered later.
    const terminal = rows.filter((row) => row.surface === "terminal");
    expect(rows).toHaveLength(50);
    expect(terminal).toHaveLength(14);
  });

  test("every row a browser draws has a label and a kind to draw it with", () => {
    for (const row of visibleSettingRows(rows)) {
      expect(row.label.length).toBeGreaterThan(0);
      expect(row.kind.type).toBeString();
    }
  });
});
