// The palette, the turn animation and the chrome glyphs are generated; this
// module only re-exports them.
//
// `src/generated/themes.ts` is serialized from the Rust in
// crates/codegen/xai-grok-pager-render/src/theme/ — the `Theme` constructors the
// pager paints from, and the `ANIMATION` constants it animates on.
// `src/generated/glyphs.ts` is serialized from the sibling `glyphs/` module —
// the functions the pager calls for every chrome character it draws. Add nothing
// here that invents a colour, a speed or a codepoint: a second copy of any of
// them is the drift the generators exist to prevent.
export {
  ANIMATION,
  ANSI16,
  DEFAULT_THEME,
  THEMES,
  type AnimationConstants,
  type Theme,
  type ThemeColor,
  type ThemeColors,
  type ThemeModifier,
  type ThemeModifiers,
  type ThemeName,
  type ThemeTerminal,
} from "./generated/themes.ts";
export { GLYPHS, type Glyphs } from "./generated/glyphs.ts";
