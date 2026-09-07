// The palette is generated; this module only re-exports it.
//
// `src/generated/themes.ts` is serialized from the Rust `Theme` constructors in
// crates/codegen/xai-grok-pager-render/src/theme/, which are what the pager
// paints from. Add nothing here that invents a color: a second palette is the
// drift the generator exists to prevent.
export {
  ANSI16,
  DEFAULT_THEME,
  THEMES,
  type Theme,
  type ThemeColor,
  type ThemeColors,
  type ThemeModifier,
  type ThemeModifiers,
  type ThemeName,
  type ThemeTerminal,
} from "./generated/themes.ts";
