//! Palette export: the one place a non-terminal renderer gets its colors from.
//!
//! ## Why the Rust struct is the definition
//!
//! Every color the pager paints comes from a [`Theme`] built by a `const fn`
//! constructor in this module's siblings ([`Theme::groknight`], [`Theme::grokday`],
//! [`Theme::tokyonight`], [`Theme::rosepine_moon`], [`Theme::oscura_midnight`],
//! [`Theme::terminal_default`]). Those constructors are the definition; this file
//! serializes them into `sdk/theme/src/generated/themes.ts` so a browser client
//! derives the same palette instead of authoring a second one.
//!
//! A second hand-written palette is exactly the failure mode this exists to
//! prevent, so nothing here re-states a color: [`color_roles`] destructures
//! `Theme` **without a `..` rest pattern**, so a new role fails to compile until
//! it is named, and [`theme_for`] matches [`ThemeKind`] exhaustively, so a new
//! theme fails to compile until it is exported. Staleness of the checked-in file
//! is caught by [`tests::generated_theme_tokens_match_the_pager_palette`], which
//! compares rather than overwrites; there is no CI to run `git diff --exit-code`.
//!
//! ## Seeds versus roles
//!
//! Each theme file does carry a small named palette (13-24 constants) and a
//! role-to-constant map, which looks like "seeds plus derivation". It is not: the
//! map differs per theme, and that difference is the theme. `md_heading_h4` takes
//! the bright gray in GrokNight, `RED` in TokyoNight, `ROSE` in Rose Pine and
//! `TEAL` in Oscura; `md_heading_h2_mod` is bold everywhere except Rose Pine,
//! which underlines it too. A derivation rule reproducing that would need a
//! per-theme table of which seed feeds which role, i.e. the enumerated role list
//! again, one indirection poorer. So the exported contract is the role list.
//!
//! ## What is deliberately not exported
//!
//! - **The quantization ladder.** [`Theme::quantized`], `windows_contrast_boost`
//!   and `ansi16_chrome_overrides` adapt a truecolor palette to terminals that
//!   cannot show it. A browser is always truecolor, so it consumes the
//!   pre-adaptation palette, which is what a truecolor terminal shows too.
//! - **Syntax highlighting.** Code spans come from syntect themes
//!   ([`crate::syntax`]), not from `Theme`; they are a separate asset.
//! - **Animation.** `wave_brightness` / `pulse_brightness` are behavior over a
//!   tick counter, not palette.

use std::fmt::Write as _;
#[cfg(test)]
use std::path::{Path, PathBuf};

use ratatui::style::{Color, Modifier};

use super::{Theme, ThemeKind};

/// Path of the generated artifact, relative to the repository root.
pub const ARTIFACT_REL_PATH: &str = "sdk/theme/src/generated/themes.ts";

/// Command that rewrites the artifact when a palette change is intended.
pub const REGENERATE_CMD: &str =
    "GROK_WRITE_THEME_TOKENS=1 cargo test -p xai-grok-pager-render theme::tokens";

/// Constructor for a theme kind.
///
/// Exhaustive on purpose: a new [`ThemeKind`] does not compile until it names a
/// constructor here, so it cannot reach the pager's picker while missing from
/// the web palette.
fn theme_for(kind: ThemeKind) -> Theme {
    match kind {
        ThemeKind::GrokNight => Theme::groknight(),
        ThemeKind::GrokDay => Theme::grokday(),
        ThemeKind::TokyoNight => Theme::tokyonight(),
        ThemeKind::RosePineMoon => Theme::rosepine_moon(),
        ThemeKind::OscuraMidnight => Theme::oscura_midnight(),
        // Meta-variant: resolved to a concrete kind before rendering, and excluded
        // from `ThemeKind::ALL`. The browser resolves it from `prefers-color-scheme`
        // against the exported `polarity` field.
        ThemeKind::Auto => Theme::groknight(),
    }
}

/// Every color role of a theme, in a fixed order, paired with its field name.
///
/// The destructuring pattern below carries no `..`: adding a field to [`Theme`]
/// is a compile error here until the field is given a token name. That is the
/// load-bearing half of the drift guard — the test only catches a stale file,
/// this catches a role the web would never have heard of.
pub fn color_roles(theme: &Theme) -> Vec<(&'static str, Color)> {
    let Theme {
        bg_base,
        bg_light,
        bg_dark,
        bg_highlight,
        bg_hover,
        bg_terminal,
        accent_user,
        accent_assistant,
        accent_thinking,
        accent_tool,
        accent_system,
        accent_error,
        accent_success,
        accent_running,
        accent_skill,
        text_primary,
        text_secondary,
        gray_dim,
        gray,
        gray_bright,
        command,
        path,
        running,
        warning,
        fuzzy_accent,
        accent_plan,
        accent_verify,
        accent_remember,
        selection_border,
        hover_border,
        prompt_border,
        prompt_border_active,
        accent_model,
        scrollbar_bg,
        scrollbar_fg,
        diff_delete_bg,
        diff_delete_fg,
        diff_insert_bg,
        diff_insert_fg,
        diff_equal_fg,
        diff_gutter_fg,
        bg_visual,
        paste_bg,
        paste_fg,
        paste_dim,
        md_heading_h1,
        md_heading_h1_mod: _,
        md_heading_h2,
        md_heading_h2_mod: _,
        md_heading_h3,
        md_heading_h3_mod: _,
        md_heading_h4,
        md_heading_h4_mod: _,
        md_heading_h5,
        md_heading_h5_mod: _,
        md_heading_h6,
        md_heading_h6_mod: _,
        md_code,
        md_task_checked,
        md_task_unchecked,
        md_muted,
        md_code_bg,
        md_text,
        link_fg,
    } = *theme;

    vec![
        ("bg_base", bg_base),
        ("bg_light", bg_light),
        ("bg_dark", bg_dark),
        ("bg_highlight", bg_highlight),
        ("bg_hover", bg_hover),
        ("bg_terminal", bg_terminal),
        ("accent_user", accent_user),
        ("accent_assistant", accent_assistant),
        ("accent_thinking", accent_thinking),
        ("accent_tool", accent_tool),
        ("accent_system", accent_system),
        ("accent_error", accent_error),
        ("accent_success", accent_success),
        ("accent_running", accent_running),
        ("accent_skill", accent_skill),
        ("text_primary", text_primary),
        ("text_secondary", text_secondary),
        ("gray_dim", gray_dim),
        ("gray", gray),
        ("gray_bright", gray_bright),
        ("command", command),
        ("path", path),
        ("running", running),
        ("warning", warning),
        ("fuzzy_accent", fuzzy_accent),
        ("accent_plan", accent_plan),
        ("accent_verify", accent_verify),
        ("accent_remember", accent_remember),
        ("selection_border", selection_border),
        ("hover_border", hover_border),
        ("prompt_border", prompt_border),
        ("prompt_border_active", prompt_border_active),
        ("accent_model", accent_model),
        ("scrollbar_bg", scrollbar_bg),
        ("scrollbar_fg", scrollbar_fg),
        ("diff_delete_bg", diff_delete_bg),
        ("diff_delete_fg", diff_delete_fg),
        ("diff_insert_bg", diff_insert_bg),
        ("diff_insert_fg", diff_insert_fg),
        ("diff_equal_fg", diff_equal_fg),
        ("diff_gutter_fg", diff_gutter_fg),
        ("bg_visual", bg_visual),
        ("paste_bg", paste_bg),
        ("paste_fg", paste_fg),
        ("paste_dim", paste_dim),
        ("md_heading_h1", md_heading_h1),
        ("md_heading_h2", md_heading_h2),
        ("md_heading_h3", md_heading_h3),
        ("md_heading_h4", md_heading_h4),
        ("md_heading_h5", md_heading_h5),
        ("md_heading_h6", md_heading_h6),
        ("md_code", md_code),
        ("md_task_checked", md_task_checked),
        ("md_task_unchecked", md_task_unchecked),
        ("md_muted", md_muted),
        ("md_code_bg", md_code_bg),
        ("md_text", md_text),
        ("link_fg", link_fg),
    ]
}

/// The per-heading text effects, in the same order as the heading colors.
pub fn modifier_roles(theme: &Theme) -> [(&'static str, Modifier); 6] {
    [
        ("md_heading_h1", theme.md_heading_h1_mod),
        ("md_heading_h2", theme.md_heading_h2_mod),
        ("md_heading_h3", theme.md_heading_h3_mod),
        ("md_heading_h4", theme.md_heading_h4_mod),
        ("md_heading_h5", theme.md_heading_h5_mod),
        ("md_heading_h6", theme.md_heading_h6_mod),
    ]
}

/// Encode a color so a browser resolves it the way the pager does.
///
/// Four forms, matching the four shapes a `ratatui::style::Color` takes in a
/// theme constructor:
///
/// - `#rrggbb` — literal 24-bit color.
/// - `ansi:N` — named ANSI slot 0-15, resolved through the exported table.
///   `Color::Gray` is slot 7 and `Color::White` slot 15, per
///   [`crate::render::color::resolve_to_rgb`].
/// - `idx:N` — 256-color palette index.
/// - `reset` — no color at all: the host's default. Only
///   [`Theme::terminal_default`] uses it.
pub fn encode_color(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(n) => format!("idx:{n}"),
        Color::Reset => "reset".to_owned(),
        Color::Black => "ansi:0".to_owned(),
        Color::Red => "ansi:1".to_owned(),
        Color::Green => "ansi:2".to_owned(),
        Color::Yellow => "ansi:3".to_owned(),
        Color::Blue => "ansi:4".to_owned(),
        Color::Magenta => "ansi:5".to_owned(),
        Color::Cyan => "ansi:6".to_owned(),
        Color::Gray => "ansi:7".to_owned(),
        Color::DarkGray => "ansi:8".to_owned(),
        Color::LightRed => "ansi:9".to_owned(),
        Color::LightGreen => "ansi:10".to_owned(),
        Color::LightYellow => "ansi:11".to_owned(),
        Color::LightBlue => "ansi:12".to_owned(),
        Color::LightMagenta => "ansi:13".to_owned(),
        Color::LightCyan => "ansi:14".to_owned(),
        Color::White => "ansi:15".to_owned(),
    }
}

/// Every ratatui text effect a theme can carry, lowest bit first.
///
/// All nine are handled rather than only the four the current themes use, so a
/// theme that reaches for `REVERSED` exports correctly instead of silently
/// dropping it.
const MODIFIER_NAMES: [(Modifier, &str); 9] = [
    (Modifier::BOLD, "bold"),
    (Modifier::DIM, "dim"),
    (Modifier::ITALIC, "italic"),
    (Modifier::UNDERLINED, "underlined"),
    (Modifier::SLOW_BLINK, "slow_blink"),
    (Modifier::RAPID_BLINK, "rapid_blink"),
    (Modifier::REVERSED, "reversed"),
    (Modifier::HIDDEN, "hidden"),
    (Modifier::CROSSED_OUT, "crossed_out"),
];

fn encode_modifier(modifier: Modifier) -> Vec<&'static str> {
    MODIFIER_NAMES
        .iter()
        .filter(|(bit, _)| modifier.contains(*bit))
        .map(|(_, name)| *name)
        .collect()
}

/// One exported palette: a [`ThemeKind`], or the terminal-native palette which
/// has no kind (it is selected by `appearance.minimal`, not by name).
struct Exported {
    name: &'static str,
    display_name: &'static str,
    /// `"dark"`, `"light"`, or `"native"` when the palette defers to the host.
    polarity: &'static str,
    requires_truecolor: bool,
    terminal_native: bool,
    theme: Theme,
}

fn exported_themes() -> Vec<Exported> {
    let mut out: Vec<Exported> = ThemeKind::ALL
        .iter()
        .map(|&kind| {
            let theme = theme_for(kind);
            Exported {
                name: kind.display_name(),
                display_name: super::display_name_for_canonical(kind.display_name()),
                // `is_dark` reads BT.709 luminance off `bg_base` pre-quantization,
                // the same test the pager uses to pick its ANSI16 polarity.
                polarity: if theme.is_dark() { "dark" } else { "light" },
                requires_truecolor: kind.requires_truecolor(),
                terminal_native: false,
                theme,
            }
        })
        .collect();

    // The terminal-native palette is every-field-`Reset`-or-named-ANSI on purpose:
    // its module doc records that polarity detection is unreliable, so it declares
    // no polarity rather than guessing one.
    out.push(Exported {
        name: "terminal-native",
        display_name: "Terminal Native",
        polarity: "native",
        requires_truecolor: false,
        terminal_native: true,
        theme: Theme::terminal_default(),
    });
    out
}

/// Render the whole artifact.
pub fn generate() -> String {
    let themes = exported_themes();
    let mut out = String::with_capacity(32 * 1024);

    out.push_str(&header());
    // The role list is the struct's, not any one theme's; the default supplies it.
    out.push_str(&type_declarations(&Theme::default()));
    out.push_str(&ansi_table());

    out.push_str("export const THEMES = {\n");
    for exported in &themes {
        out.push_str(&theme_literal(exported));
    }
    out.push_str("} as const satisfies Record<string, Theme>;\n\n");

    out.push_str("export type ThemeName = keyof typeof THEMES;\n\n");
    out.push_str(
        "/** The palette `Theme::default()` builds, and what `auto` falls back to. */\n\
         export const DEFAULT_THEME: ThemeName = \"groknight\";\n",
    );
    out
}

fn header() -> String {
    format!(
        "// @generated from crates/codegen/xai-grok-pager-render/src/theme/. \
         Do not edit this file manually.\n\
         //\n\
         // The pager builds its colors from the Rust `Theme` constructors; this file is\n\
         // serialized from those same constructors, so the two clients cannot hold\n\
         // different palettes. Editing either side alone fails\n\
         // `theme::tokens::tests::generated_theme_tokens_match_the_pager_palette`.\n\
         //\n\
         // Regenerate: {REGENERATE_CMD}\n\n"
    )
}

fn type_declarations(sample: &Theme) -> String {
    let mut out = String::new();

    out.push_str(
        "/**\n\
         \x20* A color in one of the four forms a pager theme can hold:\n\
         \x20*\n\
         \x20* - `\"#rrggbb\"` — literal 24-bit color.\n\
         \x20* - `\"ansi:N\"` — named ANSI slot 0-15; resolve through {@link ANSI16}, or\n\
         \x20*   through the host terminal's own palette when painting a terminal pane.\n\
         \x20* - `\"idx:N\"` — 256-color palette index.\n\
         \x20* - `\"reset\"` — no color: the platform default. In CSS that is `canvas` for a\n\
         \x20*   background role and `canvastext` for a foreground one. Only the\n\
         \x20*   `terminal-native` palette uses it.\n\
         \x20*/\n\
         export type ThemeColor = string;\n\n",
    );

    out.push_str("/** A ratatui text effect. */\nexport type ThemeModifier =\n");
    for (index, (_, name)) in MODIFIER_NAMES.iter().enumerate() {
        let sep = if index == 0 { ' ' } else { '|' };
        let _ = writeln!(out, "  {sep} \"{name}\"");
    }
    out.push_str(";\n\n");

    out.push_str("/** Every color role the pager paints. */\nexport interface ThemeColors {\n");
    for (role, _) in color_roles(sample) {
        let _ = writeln!(out, "  {role}: ThemeColor;");
    }
    out.push_str("}\n\n");

    out.push_str(
        "/** Extra text effects on markdown headings; the color is in {@link ThemeColors}. */\n\
         export interface ThemeModifiers {\n",
    );
    for (role, _) in modifier_roles(sample) {
        let _ = writeln!(out, "  {role}: readonly ThemeModifier[];");
    }
    out.push_str("}\n\n");

    out.push_str(
        "/**\n\
         \x20* The things a terminal answers implicitly and a browser has to be told.\n\
         \x20*\n\
         \x20* `ansi` is the standard xterm table the pager itself resolves named ANSI\n\
         \x20* against (`render::color::indexed_to_rgb`), so a terminal pane in the browser\n\
         \x20* is themed with the same 16 values the TUI assumes rather than left unstyled.\n\
         \x20* `cursor` is what the pager writes over OSC 12; `null` means it leaves the\n\
         \x20* host cursor alone.\n\
         \x20*\n\
         \x20* `muted_uses_dim` / `dim_uses_dim` reproduce `Theme::muted` and `Theme::dim`:\n\
         \x20* when the gray role is `\"reset\"` those styles de-emphasize with the SGR dim\n\
         \x20* effect instead of painting a gray, so contrast tracks the host foreground.\n\
         \x20*/\n\
         export interface ThemeTerminal {\n\
         \x20 ansi: readonly ThemeColor[];\n\
         \x20 cursor: ThemeColor | null;\n\
         \x20 selection_bg: ThemeColor;\n\
         \x20 selection_border: ThemeColor;\n\
         \x20 muted_uses_dim: boolean;\n\
         \x20 dim_uses_dim: boolean;\n\
         }\n\n",
    );

    out.push_str(
        "export interface Theme {\n\
         \x20 name: string;\n\
         \x20 display_name: string;\n\
         \x20 /** `\"native\"` declines to guess: the palette defers to the host canvas. */\n\
         \x20 polarity: \"dark\" | \"light\" | \"native\";\n\
         \x20 /** The pager falls back to `groknight` on terminals below truecolor. */\n\
         \x20 requires_truecolor: boolean;\n\
         \x20 /** Selected by `appearance.minimal`, not by name; not a picker entry. */\n\
         \x20 terminal_native: boolean;\n\
         \x20 colors: ThemeColors;\n\
         \x20 modifiers: ThemeModifiers;\n\
         \x20 terminal: ThemeTerminal;\n\
         }\n\n",
    );
    out
}

fn ansi_table() -> String {
    let mut out = String::new();
    out.push_str(
        "/**\n\
         \x20* The standard xterm ANSI 16, from `render::color::indexed_to_rgb`.\n\
         \x20*\n\
         \x20* A terminal supplies these from the user's profile and the pager never has to\n\
         \x20* name them; a browser has no profile to read, so it gets the same table the\n\
         \x20* pager's own OSC 12 path resolves named ANSI against.\n\
         \x20*/\n\
         export const ANSI16 = [\n",
    );
    for index in 0..16u8 {
        let (r, g, b) = crate::render::color::indexed_to_rgb(index);
        let _ = writeln!(out, "  \"#{r:02x}{g:02x}{b:02x}\", // {index}");
    }
    out.push_str("] as const satisfies readonly ThemeColor[];\n\n");
    out
}

fn theme_literal(exported: &Exported) -> String {
    let theme = &exported.theme;
    let mut out = String::new();

    let _ = writeln!(out, "  \"{}\": {{", exported.name);
    let _ = writeln!(out, "    name: \"{}\",", exported.name);
    let _ = writeln!(out, "    display_name: \"{}\",", exported.display_name);
    let _ = writeln!(out, "    polarity: \"{}\",", exported.polarity);
    let _ = writeln!(
        out,
        "    requires_truecolor: {},",
        exported.requires_truecolor
    );
    let _ = writeln!(out, "    terminal_native: {},", exported.terminal_native);

    out.push_str("    colors: {\n");
    for (role, color) in color_roles(theme) {
        let _ = writeln!(out, "      {role}: \"{}\",", encode_color(color));
    }
    out.push_str("    },\n");

    out.push_str("    modifiers: {\n");
    for (role, modifier) in modifier_roles(theme) {
        let names = encode_modifier(modifier)
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "      {role}: [{names}],");
    }
    out.push_str("    },\n");

    out.push_str("    terminal: {\n");
    out.push_str("      ansi: ANSI16,\n");
    // `apply_cursor_color` emits OSC 12 from `accent_user` and skips emission when
    // it resolves to nothing, which is what `null` says here.
    let cursor = match crate::render::color::resolve_to_rgb(theme.accent_user) {
        Some((r, g, b)) => format!("\"#{r:02x}{g:02x}{b:02x}\""),
        None => "null".to_owned(),
    };
    let _ = writeln!(out, "      cursor: {cursor},");
    let _ = writeln!(
        out,
        "      selection_bg: \"{}\",",
        encode_color(theme.bg_visual)
    );
    let _ = writeln!(
        out,
        "      selection_border: \"{}\",",
        encode_color(theme.selection_border)
    );
    let _ = writeln!(out, "      muted_uses_dim: {},", theme.gray == Color::Reset);
    let _ = writeln!(
        out,
        "      dim_uses_dim: {},",
        theme.gray_dim == Color::Reset
    );
    out.push_str("    },\n");

    out.push_str("  },\n");
    out
}

/// Absolute path of the checked-in artifact.
///
/// Test-only: `CARGO_MANIFEST_DIR` points into the source tree, which a shipped
/// binary has no business resolving.
#[cfg(test)]
fn artifact_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/codegen/xai-grok-pager-render.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(ARTIFACT_REL_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drift guard.
    ///
    /// It compares rather than rewrites, because a generator that silently
    /// rewrites its output only turns drift into an unreviewed diff, and this repo
    /// has no CI to run `git diff --exit-code` afterwards. Editing a theme color in
    /// Rust fails this test; hand-editing the TypeScript fails it too. Both are
    /// fixed the same way, by the command in the message.
    #[test]
    fn generated_theme_tokens_match_the_pager_palette() {
        let expected = generate();
        let path = artifact_path();

        if std::env::var_os("GROK_WRITE_THEME_TOKENS").is_some() {
            let dir = path.parent().expect("artifact path has a parent");
            std::fs::create_dir_all(dir).expect("create the generated directory");
            std::fs::write(&path, &expected).expect("write the generated palette");
            return;
        }

        let actual = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("{ARTIFACT_REL_PATH} is missing ({err}); regenerate with: {REGENERATE_CMD}")
        });
        if actual == expected {
            return;
        }

        // A whole-file diff of a few hundred generated lines buries the change, so
        // report the first line that disagrees.
        let mismatch = actual
            .lines()
            .zip(expected.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b);
        let detail = match mismatch {
            Some((index, (found, want))) => format!(
                "line {}:\n  checked in: {found}\n  from Rust:  {want}",
                index + 1
            ),
            None => format!(
                "line count differs: checked in {}, from Rust {}",
                actual.lines().count(),
                expected.lines().count()
            ),
        };
        panic!(
            "{ARTIFACT_REL_PATH} disagrees with the pager palette at {detail}\n\n\
             The Rust theme constructors are the definition. If the palette change is \
             intended, regenerate with: {REGENERATE_CMD}"
        );
    }

    /// Anchor colors of every theme, pinned by hand.
    ///
    /// The artifact test above is satisfied by regenerating, so on its own it would
    /// let an accidental color edit through as a tidy diff. These four roles per
    /// theme are the ones a user would notice changing, so they are asserted
    /// independently of the generator.
    #[test]
    fn theme_anchor_colors_are_pinned() {
        let cases: [(ThemeKind, [(&str, Color); 4]); 5] = [
            (
                ThemeKind::GrokNight,
                [
                    ("bg_base", Color::Rgb(20, 20, 20)),
                    ("text_primary", Color::Rgb(225, 225, 225)),
                    ("accent_error", Color::Rgb(247, 118, 142)),
                    ("accent_success", Color::Rgb(158, 206, 106)),
                ],
            ),
            (
                ThemeKind::GrokDay,
                [
                    ("bg_base", Color::Rgb(238, 238, 238)),
                    ("text_primary", Color::Rgb(38, 38, 38)),
                    ("accent_error", Color::Rgb(205, 48, 72)),
                    ("accent_success", Color::Rgb(55, 142, 35)),
                ],
            ),
            (
                ThemeKind::TokyoNight,
                [
                    ("bg_base", Color::Rgb(36, 40, 59)),
                    ("text_primary", Color::Rgb(192, 202, 245)),
                    ("accent_error", Color::Rgb(247, 118, 142)),
                    ("accent_success", Color::Rgb(158, 206, 106)),
                ],
            ),
            (
                ThemeKind::RosePineMoon,
                [
                    ("bg_base", Color::Rgb(35, 33, 54)),
                    ("text_primary", Color::Rgb(224, 222, 244)),
                    ("accent_error", Color::Rgb(235, 111, 146)),
                    ("accent_success", Color::Rgb(156, 207, 216)),
                ],
            ),
            (
                ThemeKind::OscuraMidnight,
                [
                    ("bg_base", Color::Rgb(3, 3, 4)),
                    ("text_primary", Color::Rgb(228, 228, 228)),
                    ("accent_error", Color::Rgb(220, 90, 100)),
                    ("accent_success", Color::Rgb(80, 180, 140)),
                ],
            ),
        ];

        for (kind, anchors) in cases {
            let roles = color_roles(&theme_for(kind));
            for (role, want) in anchors {
                let found = roles
                    .iter()
                    .find(|(name, _)| *name == role)
                    .map(|(_, color)| *color);
                assert_eq!(
                    found,
                    Some(want),
                    "{} changed its {role}",
                    kind.display_name()
                );
            }
        }
    }

    /// A theme kind that is not in `ALL` never reaches [`exported_themes`], and
    /// `ALL` is a hand-written slice, so its length is pinned the way
    /// `media_gen_limits` pins `ToolKind::VARIANT_COUNT`.
    #[test]
    fn every_theme_kind_is_exported() {
        assert_eq!(
            ThemeKind::ALL.len(),
            5,
            "ThemeKind::ALL grew/shrank; the new kind needs a `theme_for` arm and this count"
        );
        // The kinds, plus the terminal-native palette which has no kind.
        assert_eq!(exported_themes().len(), ThemeKind::ALL.len() + 1);
    }

    #[test]
    fn every_role_is_exported() {
        // `Theme` has 58 color roles and 6 heading modifiers. The destructuring in
        // `color_roles` makes a new role a compile error; this pins the count so a
        // role renamed out of the vec (but still destructured) is caught too.
        let theme = Theme::groknight();
        assert_eq!(color_roles(&theme).len(), 58);
        assert_eq!(modifier_roles(&theme).len(), 6);

        let mut names: Vec<&str> = color_roles(&theme).into_iter().map(|(n, _)| n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "a color role name is used twice");
    }

    #[test]
    fn colors_encode_to_every_form() {
        assert_eq!(encode_color(Color::Rgb(122, 162, 247)), "#7aa2f7");
        assert_eq!(encode_color(Color::Indexed(238)), "idx:238");
        assert_eq!(encode_color(Color::Reset), "reset");
        // `Gray` is ANSI 7 (silver) and `White` is ANSI 15, matching
        // `render::color::resolve_to_rgb`.
        assert_eq!(encode_color(Color::Gray), "ansi:7");
        assert_eq!(encode_color(Color::White), "ansi:15");
        assert_eq!(encode_color(Color::DarkGray), "ansi:8");
    }

    #[test]
    fn modifiers_encode_in_bit_order() {
        assert_eq!(encode_modifier(Modifier::empty()), Vec::<&str>::new());
        assert_eq!(encode_modifier(Modifier::BOLD), vec!["bold"]);
        assert_eq!(
            encode_modifier(Modifier::BOLD.union(Modifier::UNDERLINED)),
            vec!["bold", "underlined"]
        );
        assert_eq!(
            encode_modifier(Modifier::BOLD.union(Modifier::ITALIC)),
            vec!["bold", "italic"]
        );
    }

    /// The terminal-native palette is the one that carries `reset`, and the one a
    /// browser must not paint with a guessed polarity.
    #[test]
    fn terminal_native_declines_a_polarity_and_a_cursor() {
        let exported = exported_themes();
        let native = exported
            .iter()
            .find(|e| e.terminal_native)
            .expect("terminal-native palette is exported");
        assert_eq!(native.polarity, "native");
        assert_eq!(encode_color(native.theme.bg_base), "reset");
        // `apply_cursor_color` skips OSC 12 for this palette; the export says so.
        assert_eq!(
            crate::render::color::resolve_to_rgb(native.theme.accent_user),
            None
        );
        assert!(native.theme.gray == Color::Reset);
    }
}
