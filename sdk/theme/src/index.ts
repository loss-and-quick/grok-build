// The palette and the turn animation are generated; this module only re-exports
// them.
//
// `src/generated/themes.ts` is serialized from the Rust in
// crates/codegen/xai-grok-pager-render/src/theme/ — the `Theme` constructors the
// pager paints from, and the `ANIMATION` constants it animates on. Add nothing
// here that invents a colour or a speed: a second copy of either is the drift
// the generator exists to prevent.
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
