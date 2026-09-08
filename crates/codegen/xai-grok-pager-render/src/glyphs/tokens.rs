//! Glyph export: the one place a non-terminal renderer gets the pager's chrome
//! characters and the cadence they animate at.
//!
//! ## Why the Rust functions are the definition
//!
//! Every glyph the pager paints comes from a function in [`super`], and each of
//! those functions already answers the only question that has two answers —
//! legacy ConHost or everything else. So this file does not re-state a
//! codepoint: it *calls* the functions on a host where
//! [`super::is_legacy_windows_console`] is false and serializes what they
//! return into `sdk/theme/src/generated/glyphs.ts`. The alternative, which this
//! replaces, was a hand-written TypeScript copy checked by a test that scraped
//! `glyphs.rs` with a regular expression: that test could only compare the
//! glyphs somebody had already thought to list.
//!
//! ## Three guards, doing three different jobs
//!
//! 1. [`glyph_strings`], [`glyph_frames`] and [`glyph_cadences`] destructure
//!    their structs **without a `..` rest pattern**, so a field added to one of
//!    them does not compile until it is given a place on the wire. This is the
//!    same half of the guard [`crate::theme::tokens::color_roles`] carries.
//! 2. [`tests::every_public_glyph_is_accounted_for`] reads this module's
//!    sibling source and fails when it holds a `pub fn` or `pub const` that
//!    [`CENSUS`] does not mention. Guard 1 cannot see that: a new glyph
//!    function is not a new struct field, and without this the generator would
//!    stay silently one glyph behind. An author who does not want to export
//!    something says so in [`Reach::Withheld`] with a reason, which is a
//!    decision on the record rather than an omission.
//! 3. [`tests::generated_glyph_tokens_match_the_pager_glyphs`] **compares** the
//!    checked-in artifact rather than rewriting it, and prints the first line
//!    that disagrees. Rewriting is explicit, through [`REGENERATE_CMD`].
//!
//! ## What is deliberately not exported
//!
//! - **The ASCII fallbacks.** They exist because legacy ConHost does no font
//!   fallback; a browser always has one, so a client that carried them would
//!   carry a branch it can never take.
//! - **Column widths.** `PROMPT_ARROW_WIDTH` is how many terminal cells a glyph
//!   occupies. A client that lays out with CSS pays for spacing somewhere else
//!   and has nothing to apply it to.
//! - **The `char` forms of the diamonds.** They are the same codepoint as their
//!   `&str` siblings, for a caller that builds a row out of single `char`s.
//!   Exporting both would put one glyph on the wire twice.

use std::fmt::Write as _;
#[cfg(test)]
use std::path::{Path, PathBuf};

use documented::DocumentedFields;

use super::{
    MONITOR_PULSE_DIVISOR, SPINNER_DIVISOR, accent_bar, ballot_x, ballot_x_button,
    braille_spinner_frames, check_mark, chevron, chevron_down, chevron_left, chip_separator,
    collapsed_accent, copy_icon, diamond_dotted, diamond_filled, diamond_hollow, disclosure_closed,
    disclosure_open, dot_spinner_frames, enlarge, enlarge_button, filled_dot, heavy_horizontal,
    hollow_dot, is_legacy_windows_console, light_horizontal, monitor_icon_frames, prompt_arrow,
    record_dot, selection_bar, timeline_chevron_down, timeline_chevron_up, timeline_tick_active,
    timeline_tick_hover, token_arrow, warning_sign,
};

/// Path of the generated artifact, relative to the repository root.
pub const ARTIFACT_REL_PATH: &str = "sdk/theme/src/generated/glyphs.ts";

/// Command that rewrites the artifact when a glyph change is intended.
pub const REGENERATE_CMD: &str =
    "GROK_WRITE_GLYPH_TOKENS=1 cargo test -p xai-grok-pager-render glyphs::tokens";

/// How one public item of [`super`] reaches — or does not reach — the artifact.
///
/// There is no third variant on purpose. An author adding a glyph must choose,
/// and [`Reach::Withheld`] costs them a sentence saying why, which is the point:
/// "nobody got round to it" and "a browser has nothing to do with it" look
/// identical in a diff and are not the same thing.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Serialized under this key in `GLYPHS`.
    Exported(&'static str),
    /// Kept out of the artifact, for the stated reason.
    Withheld(&'static str),
}

/// Every `pub fn` and `pub const` of [`super`], and what becomes of it.
///
/// Checked against the module's own source by
/// [`tests::every_public_glyph_is_accounted_for`], so this list cannot fall
/// behind the module it describes.
#[cfg(test)]
const CENSUS: &[(&str, Reach)] = &[
    ("prompt_arrow", Reach::Exported("prompt_arrow")),
    (
        "PROMPT_ARROW_WIDTH",
        Reach::Withheld("terminal cells; a browser lays the pad out in CSS"),
    ),
    ("record_dot", Reach::Exported("record_dot_filled")),
    ("collapsed_accent", Reach::Exported("collapsed_accent")),
    ("ballot_x", Reach::Exported("ballot_x")),
    ("check_mark", Reach::Exported("check_mark")),
    ("enlarge", Reach::Exported("enlarge")),
    ("warning_sign", Reach::Exported("warning_sign")),
    ("copy_icon", Reach::Exported("copy_icon")),
    ("token_arrow", Reach::Exported("token_arrow")),
    (
        "monitor_icon_frames",
        Reach::Exported("monitor_icon_frames"),
    ),
    ("diamond_filled", Reach::Exported("diamond_filled")),
    ("diamond_hollow", Reach::Exported("diamond_hollow")),
    ("diamond_dotted", Reach::Exported("diamond_dotted")),
    (
        "diamond_filled_char",
        Reach::Withheld(
            "the codepoint of `diamond_filled`, for a caller building a row of `char`s",
        ),
    ),
    (
        "diamond_hollow_char",
        Reach::Withheld("the codepoint of `diamond_hollow`, for the same caller"),
    ),
    (
        "braille_spinner_frames",
        Reach::Exported("braille_spinner_frames"),
    ),
    ("SPINNER_DIVISOR", Reach::Exported("spinner_divisor")),
    (
        "MONITOR_PULSE_DIVISOR",
        Reach::Exported("monitor_pulse_divisor"),
    ),
    ("dot_spinner_frames", Reach::Exported("dot_spinner_frames")),
    ("chip_separator", Reach::Exported("chip_separator")),
    ("accent_bar", Reach::Exported("accent_bar")),
    (
        "timeline_chevron_up",
        Reach::Exported("timeline_chevron_up"),
    ),
    (
        "timeline_chevron_down",
        Reach::Exported("timeline_chevron_down"),
    ),
    ("heavy_horizontal", Reach::Exported("heavy_horizontal")),
    ("light_horizontal", Reach::Exported("light_horizontal")),
    (
        "timeline_tick_active",
        Reach::Exported("timeline_tick_active"),
    ),
    (
        "timeline_tick_hover",
        Reach::Exported("timeline_tick_hover"),
    ),
    ("hollow_dot", Reach::Exported("hollow_dot")),
    ("filled_dot", Reach::Exported("filled_dot")),
    ("selection_bar", Reach::Exported("selection_bar")),
    ("chevron", Reach::Exported("chevron")),
    ("chevron_left", Reach::Exported("chevron_left")),
    ("chevron_down", Reach::Exported("chevron_down")),
    ("disclosure_open", Reach::Exported("disclosure_open")),
    ("disclosure_closed", Reach::Exported("disclosure_closed")),
    ("ballot_x_button", Reach::Exported("ballot_x_button")),
    ("enlarge_button", Reach::Exported("enlarge_button")),
    (
        "legacy_glyph_fallback",
        Reach::Withheld("substitutes the ASCII stand-ins a browser never needs"),
    ),
    (
        "sanitize_toast_message",
        Reach::Withheld(
            "the same substitution plus a control-character scrub for a single terminal row",
        ),
    ),
    (
        "is_legacy_windows_console",
        Reach::Withheld("the host probe this whole module branches on, and a browser is never it"),
    ),
];

/// The single-character chrome, as a browser receives it.
///
/// Field docs are the artifact's docs: they are written here rather than
/// borrowed from [`super`], because [`super`]'s say what a legacy console gets
/// instead, and that sentence is noise to the only reader of this file.
#[derive(DocumentedFields)]
struct GlyphStrings {
    /// `❯ ` — the prompt prefix, and the marker on the selected row of a
    /// dropdown. Two terminal columns: the glyph carries its own pad, which a
    /// client laying out in CSS trims and pays for in spacing.
    prompt_arrow: &'static str,
    /// `│` — the separator between status-bar chips.
    chip_separator: &'static str,
    /// `┃` — the left accent rail beside a scrollback block or a modal panel.
    accent_bar: &'static str,
    /// `❙` — the same rail, for a block that is collapsed.
    collapsed_accent: &'static str,
    /// `▏` — the thin bar marking the selected row of a list.
    selection_bar: &'static str,
    /// `◆` — the bullet on a tool call, a collapsed thinking block, and a used
    /// cell of the context bar.
    diamond_filled: &'static str,
    /// `◇` — its hollow partner: a free cell of the context bar, an idle row.
    diamond_hollow: &'static str,
    /// `◈` — the verb-group header diamond, and the context bar's tool
    /// definitions.
    diamond_dotted: &'static str,
    /// `●` — the chosen option of a radio group, and the marker on a selected
    /// list row.
    filled_dot: &'static str,
    /// `○` — the unchosen option of that same group.
    hollow_dot: &'static str,
    /// `✓` — the done marker.
    check_mark: &'static str,
    /// `✗` — its failure sibling, and the close / cancel / stop button.
    ballot_x: &'static str,
    /// `[✗]` — the pre-composed bracketed button form of `ballot_x`.
    ballot_x_button: &'static str,
    /// `⚠` — the "blocked on you" marker the terminal title carries as
    /// `⚠ Action Required` while a permission request is unanswered.
    warning_sign: &'static str,
    /// `↗` — the enlarge / view button.
    enlarge: &'static str,
    /// `[↗]` — its pre-composed bracketed button form.
    enlarge_button: &'static str,
    /// `⧉` — the copy button.
    copy_icon: &'static str,
    /// `⇣` — the context-token count in the turn-status line.
    token_arrow: &'static str,
    /// `›` — the collapsed fold indicator, the settings breadcrumb separator,
    /// and "next".
    chevron: &'static str,
    /// `‹` — its mirror: "previous", and the way back up a path.
    chevron_left: &'static str,
    /// `⌄` — the downward member of the same family, at `chevron`'s light
    /// weight rather than the solid `disclosure_open` triangle.
    chevron_down: &'static str,
    /// `▾` — an expanded section: its rows are visible below the header.
    disclosure_open: &'static str,
    /// `▸` — a collapsed section, and so a directory you can descend into.
    disclosure_closed: &'static str,
    /// `▴` — the timeline rail's previous-turn chevron.
    timeline_chevron_up: &'static str,
    /// `▾` — the timeline rail's next-turn chevron.
    timeline_chevron_down: &'static str,
    /// `━` — a heavy horizontal rule.
    heavy_horizontal: &'static str,
    /// `─` — a light one, and the timeline rail's idle tick.
    light_horizontal: &'static str,
    /// `━━` — the timeline rail's active tick, pre-composed at two columns.
    timeline_tick_active: &'static str,
    /// `──` — the same tick, hovered.
    timeline_tick_hover: &'static str,
    /// `◉` — the bright half of the voice-capture indicator's pulse.
    record_dot_filled: &'static str,
    /// `◎` — its dim half. The pair reads as a studio recording light.
    record_dot_open: &'static str,
}

/// The animated glyph cycles.
#[derive(DocumentedFields)]
struct GlyphFrames {
    /// `⠋⠙⠹⠸⠼⠴⠦⠧` — the progress spinner, one frame every
    /// `spinner_divisor` ticks.
    braille_spinner_frames: &'static [&'static str],
    /// `⋅ : ⸬ ⁙` — the quieter dot spinner, twice round per cycle.
    dot_spinner_frames: &'static [&'static str],
    /// `○ ◎ ◉ ◎` — the "still running in the background" pulse, one frame
    /// every `monitor_pulse_divisor` ticks.
    monitor_icon_frames: &'static [&'static str],
}

/// How long each frame of a cycle is held, in animation ticks.
///
/// Ticks, not milliseconds: `ANIMATION.ticks_per_second` in the palette
/// artifact turns one into the other, and it is the same counter both files
/// mean.
#[derive(DocumentedFields)]
struct GlyphCadences {
    /// Ticks per frame of `braille_spinner_frames`.
    spinner_divisor: u64,
    /// Ticks per frame of `monitor_icon_frames`: twice the spinner's dwell, so
    /// the idle cue breathes rather than races.
    monitor_pulse_divisor: u64,
}

/// The exported strings, in artifact order, with the docs that describe them.
///
/// Destructured without a `..` rest pattern: a glyph added to [`GlyphStrings`]
/// is a compile error here until it is given a name on the wire.
fn glyph_strings() -> Vec<(&'static str, &'static str, &'static str)> {
    let GlyphStrings {
        prompt_arrow: arrow,
        chip_separator: chip,
        accent_bar: rail,
        collapsed_accent: collapsed,
        selection_bar: selection,
        diamond_filled: filled,
        diamond_hollow: hollow,
        diamond_dotted: dotted,
        filled_dot: dot_on,
        hollow_dot: dot_off,
        check_mark: check,
        ballot_x: cross,
        ballot_x_button: cross_button,
        enlarge: grow,
        warning_sign: warning,
        enlarge_button: grow_button,
        copy_icon: copy,
        token_arrow: tokens,
        chevron: right,
        chevron_left: left,
        chevron_down: down,
        disclosure_open: open,
        disclosure_closed: closed,
        timeline_chevron_up: rail_up,
        timeline_chevron_down: rail_down,
        heavy_horizontal: heavy,
        light_horizontal: light,
        timeline_tick_active: tick_active,
        timeline_tick_hover: tick_hover,
        record_dot_filled: record_on,
        record_dot_open: record_off,
    } = exported_strings();

    [
        ("prompt_arrow", arrow),
        ("chip_separator", chip),
        ("accent_bar", rail),
        ("collapsed_accent", collapsed),
        ("selection_bar", selection),
        ("diamond_filled", filled),
        ("diamond_hollow", hollow),
        ("diamond_dotted", dotted),
        ("filled_dot", dot_on),
        ("hollow_dot", dot_off),
        ("check_mark", check),
        ("ballot_x", cross),
        ("ballot_x_button", cross_button),
        ("enlarge", grow),
        ("warning_sign", warning),
        ("enlarge_button", grow_button),
        ("copy_icon", copy),
        ("token_arrow", tokens),
        ("chevron", right),
        ("chevron_left", left),
        ("chevron_down", down),
        ("disclosure_open", open),
        ("disclosure_closed", closed),
        ("timeline_chevron_up", rail_up),
        ("timeline_chevron_down", rail_down),
        ("heavy_horizontal", heavy),
        ("light_horizontal", light),
        ("timeline_tick_active", tick_active),
        ("timeline_tick_hover", tick_hover),
        ("record_dot_filled", record_on),
        ("record_dot_open", record_off),
    ]
    .into_iter()
    .map(|(name, value)| (name, docs_of::<GlyphStrings>(name), value))
    .collect()
}

/// The exported cycles, destructured for the reason [`glyph_strings`] is.
fn glyph_frames() -> Vec<(&'static str, &'static str, &'static [&'static str])> {
    let GlyphFrames {
        braille_spinner_frames: braille,
        dot_spinner_frames: dots,
        monitor_icon_frames: monitor,
    } = GlyphFrames {
        braille_spinner_frames: braille_spinner_frames(),
        dot_spinner_frames: dot_spinner_frames(),
        monitor_icon_frames: monitor_icon_frames(),
    };

    [
        ("braille_spinner_frames", braille),
        ("dot_spinner_frames", dots),
        ("monitor_icon_frames", monitor),
    ]
    .into_iter()
    .map(|(name, value)| (name, docs_of::<GlyphFrames>(name), value))
    .collect()
}

/// The exported dwells, destructured for the reason [`glyph_strings`] is.
fn glyph_cadences() -> Vec<(&'static str, &'static str, u64)> {
    let GlyphCadences {
        spinner_divisor: spinner,
        monitor_pulse_divisor: monitor,
    } = GlyphCadences {
        spinner_divisor: SPINNER_DIVISOR,
        monitor_pulse_divisor: MONITOR_PULSE_DIVISOR,
    };

    [
        ("spinner_divisor", spinner),
        ("monitor_pulse_divisor", monitor),
    ]
    .into_iter()
    .map(|(name, value)| (name, docs_of::<GlyphCadences>(name), value))
    .collect()
}

/// Field docs, or a panic naming the field that has none.
///
/// A silent empty doc would ship an artifact whose entries explain nothing,
/// which is most of what the artifact is for.
fn docs_of<T: DocumentedFields>(field: &str) -> &'static str {
    match T::get_field_docs(field) {
        Ok(docs) => docs,
        Err(_) => panic!("`{field}` needs a doc comment to export"),
    }
}

/// Read every glyph off the functions that define it.
///
/// Nothing here writes a codepoint; `record_dot` is called twice because its
/// two states are one function with a flag rather than two functions.
fn exported_strings() -> GlyphStrings {
    GlyphStrings {
        prompt_arrow: prompt_arrow(),
        chip_separator: chip_separator(),
        accent_bar: accent_bar(),
        collapsed_accent: collapsed_accent(),
        selection_bar: selection_bar(),
        diamond_filled: diamond_filled(),
        diamond_hollow: diamond_hollow(),
        diamond_dotted: diamond_dotted(),
        filled_dot: filled_dot(),
        hollow_dot: hollow_dot(),
        check_mark: check_mark(),
        ballot_x: ballot_x(),
        ballot_x_button: ballot_x_button(),
        enlarge: enlarge(),
        warning_sign: warning_sign(),
        enlarge_button: enlarge_button(),
        copy_icon: copy_icon(),
        token_arrow: token_arrow(),
        chevron: chevron(),
        chevron_left: chevron_left(),
        chevron_down: chevron_down(),
        disclosure_open: disclosure_open(),
        disclosure_closed: disclosure_closed(),
        timeline_chevron_up: timeline_chevron_up(),
        timeline_chevron_down: timeline_chevron_down(),
        heavy_horizontal: heavy_horizontal(),
        light_horizontal: light_horizontal(),
        timeline_tick_active: timeline_tick_active(),
        timeline_tick_hover: timeline_tick_hover(),
        record_dot_filled: record_dot(true),
        record_dot_open: record_dot(false),
    }
}

/// A TypeScript string literal holding exactly the codepoints of `value`.
///
/// Non-ASCII is escaped as `\u{…}`, the same form `glyphs.rs` writes, so the
/// two files can be read side by side. The literal glyph is in the JSDoc above
/// it, which is where a person wants to see it and a diff tool will not mangle
/// it.
fn ts_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(ch),
            other => {
                let _ = write!(out, "\\u{{{:04X}}}", other as u32);
            }
        }
    }
    out.push('"');
    out
}

/// Render a Rust doc comment as JSDoc at the given indent.
fn jsdoc(docs: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let lines: Vec<&str> = docs.lines().map(str::trim).collect();
    if let [only] = lines.as_slice() {
        return format!("{pad}/** {only} */\n");
    }

    let mut out = format!("{pad}/**\n");
    for line in lines {
        if line.is_empty() {
            let _ = writeln!(out, "{pad} *");
        } else {
            let _ = writeln!(out, "{pad} * {line}");
        }
    }
    let _ = writeln!(out, "{pad} */");
    out
}

fn header() -> String {
    format!(
        "// @generated from crates/codegen/xai-grok-pager-render/src/glyphs/. \
         Do not edit this file manually.\n\
         //\n\
         // The pager draws its chrome from the Rust functions in that module; this file is\n\
         // the value each of them returns on a host that is not a legacy Windows console,\n\
         // so the two clients cannot hold different glyphs. Editing either side alone fails\n\
         // `glyphs::tokens::tests::generated_glyph_tokens_match_the_pager_glyphs`.\n\
         //\n\
         // Regenerate: {REGENERATE_CMD}\n\n"
    )
}

/// Render the whole artifact.
pub fn generate() -> String {
    // Every value below is whatever the glyph functions return in *this*
    // process, and they answer differently under `GROK_FORCE_LEGACY_CONSOLE=1`.
    // Generating there would silently ship the ASCII stand-ins to a browser
    // that has no ConHost to need them, so refuse rather than emit them.
    assert!(
        !is_legacy_windows_console(),
        "the glyph artifact is the modern glyph set; unset GROK_FORCE_LEGACY_CONSOLE and rerun"
    );

    let mut out = String::with_capacity(8 * 1024);
    out.push_str(&header());

    out.push_str(
        "/**\n\
         \x20* The chrome glyphs the pager paints, and the cadence its animated ones run at.\n\
         \x20*\n\
         \x20* These are the modern set. `glyphs.rs` also carries an ASCII stand-in for each\n\
         \x20* one, for legacy Windows consoles that do no font fallback; a browser always\n\
         \x20* has one, so that branch is not on the wire.\n\
         \x20*/\n\
         export interface Glyphs {\n",
    );
    for (name, docs, _) in glyph_strings() {
        out.push_str(&jsdoc(docs, 2));
        let _ = writeln!(out, "  {name}: string;");
    }
    for (name, docs, _) in glyph_frames() {
        out.push_str(&jsdoc(docs, 2));
        let _ = writeln!(out, "  {name}: readonly string[];");
    }
    for (name, docs, _) in glyph_cadences() {
        out.push_str(&jsdoc(docs, 2));
        let _ = writeln!(out, "  {name}: number;");
    }
    out.push_str("}\n\n");

    out.push_str("export const GLYPHS = {\n");
    for (name, _, value) in glyph_strings() {
        let _ = writeln!(out, "  {name}: {},", ts_string(value));
    }
    for (name, _, frames) in glyph_frames() {
        // One frame a line, the way the palette lists ANSI slots: a cycle is
        // read frame by frame, and the index comment is what makes an
        // out-of-order edit visible.
        let _ = writeln!(out, "  {name}: [");
        for (index, frame) in frames.iter().enumerate() {
            let _ = writeln!(out, "    {}, // {index}", ts_string(frame));
        }
        out.push_str("  ],\n");
    }
    for (name, _, value) in glyph_cadences() {
        let _ = writeln!(out, "  {name}: {value},");
    }
    out.push_str("} as const satisfies Glyphs;\n");
    out
}

/// Absolute path of the checked-in artifact.
///
/// Test-only, for the reason [`crate::theme::tokens`]'s twin is: the manifest
/// directory points into the source tree, which a shipped binary has no
/// business resolving.
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
    use std::collections::BTreeSet;

    /// The census guard.
    ///
    /// The struct destructures catch a field nobody exported; they cannot catch
    /// a *function* nobody turned into a field, and that is the failure the
    /// hand-written TypeScript this generator replaces had lived with. So the
    /// module's own source is read and every public item in it must appear in
    /// [`CENSUS`], exported or withheld with a reason.
    #[test]
    fn every_public_glyph_is_accounted_for() {
        const SOURCE: &str = include_str!("mod.rs");

        let mut declared: BTreeSet<&str> = BTreeSet::new();
        for line in SOURCE.lines() {
            let line = line.trim_start();
            let name = line
                .strip_prefix("pub fn ")
                .or_else(|| line.strip_prefix("pub const fn "))
                .and_then(|rest| rest.split('(').next())
                .or_else(|| {
                    line.strip_prefix("pub const ")
                        .and_then(|rest| rest.split(':').next())
                });
            if let Some(name) = name {
                declared.insert(name.trim());
            }
        }

        let censused: BTreeSet<&str> = CENSUS.iter().map(|(name, _)| *name).collect();
        let missing: Vec<&&str> = declared.difference(&censused).collect();
        assert!(
            missing.is_empty(),
            "glyphs.rs declares {missing:?}, which the census does not mention. \
             Add each one to `CENSUS`: `Reach::Exported` with a field on `GlyphStrings`, \
             `GlyphFrames` or `GlyphCadences`, or `Reach::Withheld` with the reason a \
             browser has no use for it."
        );
        let stale: Vec<&&str> = censused.difference(&declared).collect();
        assert!(
            stale.is_empty(),
            "the census names {stale:?}, which glyphs.rs no longer declares"
        );

        // A blank reason is the omission this variant exists to prevent, so it
        // is not a valid way to fill the line in.
        for (item, reach) in CENSUS {
            if let Reach::Withheld(reason) = reach {
                assert!(
                    !reason.trim().is_empty(),
                    "`{item}` is withheld without saying why"
                );
            }
        }
    }

    /// Every `Reach::Exported` key must actually be in the artifact.
    ///
    /// Without this the census could claim an export that no struct field
    /// backs, which is exactly the kind of documentation that reads true and is
    /// not.
    #[test]
    fn every_exported_census_key_is_on_the_wire() {
        let on_the_wire: BTreeSet<&str> = glyph_strings()
            .into_iter()
            .map(|(name, _, _)| name)
            .chain(glyph_frames().into_iter().map(|(name, _, _)| name))
            .chain(glyph_cadences().into_iter().map(|(name, _, _)| name))
            .collect();

        for (item, reach) in CENSUS {
            if let Reach::Exported(key) = reach {
                assert!(
                    on_the_wire.contains(key),
                    "the census exports `{item}` as `{key}`, which no struct field produces"
                );
            }
        }

        // `record_dot` is one function and two states, so the wire carries one
        // key the census cannot name. Nothing else may be.
        let claimed: BTreeSet<&str> = CENSUS
            .iter()
            .filter_map(|(_, reach)| match reach {
                Reach::Exported(key) => Some(*key),
                Reach::Withheld(_) => None,
            })
            .collect();
        let unclaimed: Vec<&&str> = on_the_wire.difference(&claimed).collect();
        assert_eq!(
            unclaimed,
            [&"record_dot_open"],
            "a key on the wire that no census line accounts for"
        );
    }

    /// The drift guard.
    ///
    /// It compares rather than rewrites, because a generator that silently
    /// rewrites its output only turns drift into an unreviewed diff, and this
    /// repo has no CI to run `git diff --exit-code` afterwards. Changing a
    /// glyph in Rust fails this test; hand-editing the TypeScript fails it too.
    /// Both are fixed the same way, by the command in the message.
    #[test]
    fn generated_glyph_tokens_match_the_pager_glyphs() {
        let expected = generate();
        let path = artifact_path();

        if std::env::var_os("GROK_WRITE_GLYPH_TOKENS").is_some() {
            let dir = path.parent().expect("artifact path has a parent");
            std::fs::create_dir_all(dir).expect("create the generated directory");
            std::fs::write(&path, &expected).expect("write the generated glyphs");
            return;
        }

        let actual = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("{ARTIFACT_REL_PATH} is missing ({err}); regenerate with: {REGENERATE_CMD}")
        });
        if actual == expected {
            return;
        }

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
            "{ARTIFACT_REL_PATH} disagrees with the pager glyphs at {detail}\n\n\
             The Rust glyph functions are the definition. If the glyph change is \
             intended, regenerate with: {REGENERATE_CMD}"
        );
    }

    /// Anchor glyphs, pinned by hand.
    ///
    /// The artifact test above is satisfied by regenerating, so on its own it
    /// would let an accidental edit through as a tidy diff. These are the
    /// glyphs a user would notice changing, so they are asserted independently
    /// of the generator — the same second line the palette's anchor colors are.
    #[test]
    fn anchor_glyphs_are_pinned() {
        let by_name: std::collections::BTreeMap<&str, &str> = glyph_strings()
            .into_iter()
            .map(|(name, _, value)| (name, value))
            .collect();

        for (name, expected) in [
            ("prompt_arrow", "\u{276F} "),
            ("diamond_filled", "\u{25C6}"),
            ("accent_bar", "\u{2503}"),
            ("check_mark", "\u{2713}"),
            ("ballot_x", "\u{2717}"),
            ("chip_separator", "\u{2502}"),
            ("hollow_dot", "\u{25CB}"),
        ] {
            assert_eq!(by_name.get(name), Some(&expected), "glyph `{name}`");
        }
    }

    /// The escaper writes codepoints, not bytes, and leaves ASCII alone.
    #[test]
    fn ts_string_escapes_the_glyph_and_keeps_the_pad() {
        assert_eq!(ts_string("\u{276F} "), "\"\\u{276F} \"");
        assert_eq!(ts_string("[\u{2717}]"), "\"[\\u{2717}]\"");
        assert_eq!(ts_string("\""), "\"\\\"\"");
        assert_eq!(ts_string("\\"), "\"\\\\\"");
    }
}
