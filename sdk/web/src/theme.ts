// Every colour in this client comes from `@grok-build/theme`, which is generated
// from the pager's own Rust `Theme` constructors. Nothing here names a colour.
//
// What this module *does* own is the two translations a browser needs and a
// terminal does not:
//
//   - `ThemeColor` is a tagged string (`"#rrggbb"`, `"ansi:N"`, `"idx:N"`,
//     `"reset"`); CSS wants a colour. The mapping for `reset` is dictated by the
//     generated file's own doc comment: `canvas` for a background role,
//     `canvastext` for a foreground one.
//   - a role is a background or a foreground, and only the browser has to care.
//     `BACKGROUND_ROLES` says which, and `everyRoleIsClassified` in the tests
//     fails when a new generated role lands unclassified.
import { ANSI16, THEMES, type Theme, type ThemeColor, type ThemeColors } from "@grok-build/theme";

export type ThemeRole = keyof ThemeColors;

/**
 * Roles painted as a background.
 *
 * Only these turn a `"reset"` into `canvas`; every other role becomes
 * `canvastext`. The list is spelled out rather than inferred from the `bg_`
 * prefix because `diff_delete_bg`, `md_code_bg` and `paste_bg` are backgrounds
 * with the marker on the other end, and `scrollbar_bg` is one with a `_fg`
 * sibling — a prefix rule would silently mis-paint four roles.
 */
export const BACKGROUND_ROLES: ReadonlySet<ThemeRole> = new Set<ThemeRole>([
  "bg_base",
  "bg_light",
  "bg_dark",
  "bg_highlight",
  "bg_hover",
  "bg_terminal",
  "bg_visual",
  "scrollbar_bg",
  "diff_delete_bg",
  "diff_insert_bg",
  "paste_bg",
  "md_code_bg",
]);

/** The platform default a `"reset"` resolves to, per the generated file's doc comment. */
export function resetColorFor(role: ThemeRole): string {
  return BACKGROUND_ROLES.has(role) ? "canvas" : "canvastext";
}

/**
 * Resolve one {@link ThemeColor} to a CSS colour, or `null` when the generated
 * artifact does not carry the value it names.
 *
 * `null` happens for exactly one form today: `"idx:N"`, the 256-colour palette.
 * The pager resolves those against `render::color::indexed_to_rgb`, and that
 * table is not exported — only its first 16 entries are, as {@link ANSI16}. No
 * shipped theme uses `idx:`, and `noThemeNeedsThe256ColorTable` in the tests
 * fails the day one does, rather than letting a client guess the cube.
 */
export function resolveThemeColor(color: ThemeColor): string | null {
  if (color === "reset") return null;
  if (color.startsWith("#")) return color;
  const ansi = /^ansi:(\d+)$/.exec(color);
  if (ansi) return ANSI16[Number(ansi[1])] ?? null;
  return null;
}

/** CSS custom-property name for a role: `gray_bright` becomes `--grok-gray-bright`. */
export function cssVarName(role: ThemeRole): string {
  return `--grok-${role.replaceAll("_", "-")}`;
}

/** Every role of a theme as CSS custom-property declarations. */
export function themeCssVariables(theme: Theme): Record<string, string> {
  const vars: Record<string, string> = {};
  for (const [role, color] of Object.entries(theme.colors) as [ThemeRole, ThemeColor][]) {
    vars[cssVarName(role)] = resolveThemeColor(color) ?? resetColorFor(role);
  }
  vars["--grok-selection-bg"] =
    resolveThemeColor(theme.terminal.selection_bg) ?? resetColorFor("bg_visual");
  vars["--grok-cursor"] = theme.terminal.cursor
    ? (resolveThemeColor(theme.terminal.cursor) ?? resetColorFor("accent_user"))
    : "var(--grok-accent-user)";
  for (const [i, color] of theme.terminal.ansi.entries()) {
    vars[`--grok-ansi-${i}`] = resolveThemeColor(color) ?? "canvastext";
  }
  return vars;
}

/**
 * Opacity standing in for the SGR dim effect.
 *
 * `Theme::muted` de-emphasizes with dim instead of painting a gray when a
 * palette's gray role is `"reset"` — which is `terminal-native`, and only it.
 * The generated artifact carries the *flag* but no ratio, because a terminal
 * does not need one, so the number is chosen here.
 *
 * It is derived rather than picked by eye. The five palettes that do name a
 * gray span 2.17:1 (tokyonight) to 5.64:1 (oscura-midnight) against their own
 * backgrounds, so that range is what this product already calls "muted". The
 * worst case a browser can produce is a `"reset"` foreground on a `"reset"`
 * background — pure black on pure white, or the inverse — and 0.75 lands that
 * at 3.5:1, mid-range and clear of the 3:1 floor for large text. The earlier
 * 0.6 put it at 2.33:1, which a browser reader cannot correct the way a
 * terminal user corrects dim in their profile.
 *
 * `mutedContrastOnPlainCanvas` and its test pin that arithmetic.
 */
export const DIM_OPACITY = 0.75;

export function mutedOpacity(theme: Theme): string {
  return theme.terminal.muted_uses_dim ? String(DIM_OPACITY) : "1";
}

/**
 * Contrast of dimmed `"reset"` text on a plain `"reset"` canvas.
 *
 * Composited against an opaque background, `opacity` is a linear blend, so the
 * dimmed foreground's relative luminance is `1 - opacity` for black on white.
 */
export function mutedContrastOnPlainCanvas(opacity: number): number {
  const dimmed = 1 - opacity;
  return 1.05 / (dimmed + 0.05);
}

export function applyTheme(root: HTMLElement, theme: Theme): void {
  for (const [name, value] of Object.entries(themeCssVariables(theme))) {
    root.style.setProperty(name, value);
  }
  root.style.setProperty("--grok-muted-opacity", mutedOpacity(theme));
  root.dataset["grokTheme"] = theme.name;
  root.style.colorScheme = theme.polarity === "native" ? "light dark" : theme.polarity;
}

export function themeByName(name: string): Theme {
  const themes: Record<string, Theme> = THEMES;
  return themes[name] ?? THEMES["groknight"];
}
