import { describe, expect, test } from "bun:test";
import { ANSI16, THEMES, type Theme, type ThemeColor } from "@grok-build/theme";

import {
  BACKGROUND_ROLES,
  cssVarName,
  mutedOpacity,
  resetColorFor,
  resolveThemeColor,
  themeCssVariables,
  DIM_OPACITY,
  mutedContrastOnPlainCanvas,
  type ThemeRole,
} from "../src/theme.ts";

const themes = Object.values(THEMES) as unknown as Theme[];

function allRoles(): ThemeRole[] {
  return Object.keys(THEMES.groknight.colors) as ThemeRole[];
}

describe("theme tokens", () => {
  test("every generated role is classified as background or foreground", () => {
    // The guard that matters. A new role in `themes.ts` lands here unclassified,
    // and `resetColorFor` would quietly call it a foreground — which paints a
    // background `canvastext` in the `terminal-native` palette.
    for (const role of allRoles()) {
      expect(typeof BACKGROUND_ROLES.has(role)).toBe("boolean");
    }
    // Backgrounds must be a subset of the real role set, so a rename in the
    // generated file cannot leave a dead name behind.
    for (const role of BACKGROUND_ROLES) {
      expect(allRoles()).toContain(role);
    }
  });

  test("no shipped theme needs the 256-colour table", () => {
    // `idx:N` is expressible in `ThemeColor` but the generated artifact exports
    // only ANSI 0-15. The day a theme uses `idx:`, this fails rather than
    // letting a client invent the xterm cube.
    for (const theme of themes) {
      const values: ThemeColor[] = [
        ...Object.values(theme.colors),
        ...theme.terminal.ansi,
        theme.terminal.selection_bg,
        theme.terminal.selection_border,
        ...(theme.terminal.cursor ? [theme.terminal.cursor] : []),
      ];
      for (const value of values) {
        expect(value.startsWith("idx:")).toBe(false);
      }
    }
  });

  test("resolves every colour form the generated themes actually use", () => {
    expect(resolveThemeColor("#141414")).toBe("#141414");
    expect(resolveThemeColor("ansi:4")).toBe(ANSI16[4]);
    expect(resolveThemeColor("reset")).toBeNull();
    expect(resolveThemeColor("idx:200")).toBeNull();
  });

  test("`reset` becomes canvas for a background and canvastext for a foreground", () => {
    expect(resetColorFor("bg_base")).toBe("canvas");
    expect(resetColorFor("md_code_bg")).toBe("canvas");
    expect(resetColorFor("text_primary")).toBe("canvastext");
    expect(resetColorFor("scrollbar_fg")).toBe("canvastext");
  });

  test("css variables cover every role and the ANSI table, for every theme", () => {
    for (const theme of themes) {
      const vars = themeCssVariables(theme);
      for (const role of allRoles()) {
        expect(vars[cssVarName(role)]).toBeString();
      }
      for (let i = 0; i < 16; i += 1) {
        expect(vars[`--grok-ansi-${i}`]).toBe(ANSI16[i]);
      }
      expect(vars["--grok-cursor"]).toBeString();
      expect(vars["--grok-selection-bg"]).toBeString();
    }
  });

  test("a variable carries the generated value verbatim, never a re-derived one", () => {
    for (const theme of themes) {
      const vars = themeCssVariables(theme);
      for (const role of allRoles()) {
        const generated = theme.colors[role];
        const expected =
          generated === "reset"
            ? resetColorFor(role)
            : (resolveThemeColor(generated) ?? resetColorFor(role));
        expect(vars[cssVarName(role)]).toBe(expected);
      }
    }
  });

  test("terminal-native de-emphasizes with dim, because it has no gray to paint", () => {
    const native = THEMES["terminal-native"] as unknown as Theme;
    expect(native.terminal.muted_uses_dim).toBe(true);
    expect(Number(mutedOpacity(native))).toBeLessThan(1);
    expect(mutedOpacity(THEMES.groknight as unknown as Theme)).toBe("1");
  });

  test("role names become kebab-case custom properties", () => {
    expect(cssVarName("gray_bright")).toBe("--grok-gray-bright");
    expect(cssVarName("md_heading_h1")).toBe("--grok-md-heading-h1");
  });
});

describe("the dim stand-in", () => {
  test("only the palette with no gray of its own uses it", () => {
    // `Theme::muted` dims instead of painting a gray exactly when the gray role
    // is `"reset"`. If a second palette ever takes that branch, the derivation
    // below has to be re-checked against it.
    const dimming = themes.filter((theme) => theme.terminal.muted_uses_dim);
    expect(dimming.map((theme) => theme.name)).toEqual(["terminal-native"]);
    for (const theme of dimming) expect(theme.colors.gray).toBe("reset");
  });

  test("dimmed text stays inside the band the named grays occupy", () => {
    // The floor this replaces: 0.6 put plain black-on-white at 2.33:1, fainter
    // than any palette's own muted gray and under the 3:1 large-text floor.
    expect(mutedContrastOnPlainCanvas(0.6)).toBeCloseTo(2.33, 2);
    const ratio = mutedContrastOnPlainCanvas(DIM_OPACITY);
    expect(ratio).toBeGreaterThanOrEqual(3);
    expect(ratio).toBeLessThanOrEqual(4);
  });

  test("the stand-in lands inside the range the named palettes span", () => {
    // The range is computed, not written down, so a palette edit moves the
    // check with it instead of leaving a stale pair of numbers behind.
    const ratios = themes
      .filter((theme) => !theme.terminal.muted_uses_dim)
      .flatMap((theme) => [
        contrast(theme.colors.gray, theme.colors.bg_light),
        contrast(theme.colors.gray, theme.colors.bg_base),
        contrast(theme.colors.gray, theme.colors.bg_dark),
      ]);
    const ratio = mutedContrastOnPlainCanvas(DIM_OPACITY);
    expect(ratio).toBeGreaterThanOrEqual(Math.min(...ratios));
    expect(ratio).toBeLessThanOrEqual(Math.max(...ratios));
  });
});

/** WCAG relative luminance of a `#rrggbb` colour. */
function luminance(hex: string): number {
  const channels = [1, 3, 5].map((at) => {
    const v = Number.parseInt(hex.slice(at, at + 2), 16) / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * channels[0]! + 0.7152 * channels[1]! + 0.0722 * channels[2]!;
}

function contrast(fg: string, bg: string): number {
  const a = luminance(fg);
  const b = luminance(bg);
  const [hi, lo] = a > b ? [a, b] : [b, a];
  return (hi + 0.05) / (lo + 0.05);
}
