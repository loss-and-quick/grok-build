//! Turn animation: the curves the pager animates over, and the numbers driving them.
//!
//! ## Why this sits beside the palette
//!
//! The palette answers *what colour*; this answers *how bright, this frame*.
//! Both are the pager's alone, and a second client that derives one and
//! hand-copies the other has only moved the drift somewhere less visible. So
//! the constants live here, next to the curves that read them, and
//! [`super::tokens`] serialises both into one generated artifact under one
//! guard.
//!
//! ## What stays a user knob
//!
//! `AnimationConfig::fps` and `AnimationConfig::wave_rows` are read from
//! `pager.toml`, not from here. The first is the frame cadence, the second is
//! how far the wave's phase travels per *terminal row* — between them, exactly
//! what `settings::registry::SettingSurface::Terminal` names as terminal
//! front-end tuning ("a column width, the frame cadence"). Only the shipped
//! cadence is exported, and only so a browser has a tick to count in;
//! `wave_rows` is not, because a browser has no row grid for it to mean
//! anything in.

use documented::DocumentedFields;

/// Every constant the pager's turn animation runs on.
///
/// A struct rather than loose `const`s so [`super::tokens`] can destructure it
/// **without a `..` rest pattern**: a knob added here does not compile until it
/// has a name on the wire. That is the same load-bearing half of the drift
/// guard `tokens::color_roles` carries for colour roles — the test catches a
/// stale artifact, the pattern catches a number the browser never heard of.
#[derive(Debug, Clone, Copy, PartialEq, DocumentedFields)]
pub struct AnimationConstants {
    /// Radians per tick for the running accent wave: tool rails, running
    /// bullets, and the running verb-group diamond.
    ///
    /// A rail travels one full wave every `2π / wave_speed` ticks.
    pub wave_speed: f32,
    /// Radians per tick for every "waiting on you" diamond, whatever is being
    /// waited on.
    ///
    /// The pulse is `sin²`, whose period is π, so at 30 ticks per second this
    /// is about a 1.3s cycle.
    pub waiting_pulse_speed: f32,
    /// Brightness the waiting pulse never drops below.
    ///
    /// The diamond dims at the trough instead of going dark, which is what
    /// keeps "paused on you" legible.
    pub waiting_floor: f32,
    /// Brightness the waiting pulse adds above the floor at its peak; floor
    /// plus range is full accent.
    pub waiting_range: f32,
}

/// The pager's turn animation, as shipped.
pub const ANIMATION: AnimationConstants = AnimationConstants {
    wave_speed: 0.15,
    waiting_pulse_speed: 0.08,
    waiting_floor: 0.3,
    waiting_range: 0.7,
};

/// Compute animated brightness for a wave traveling along the accent line.
///
/// Each row has a fixed phase offset (`wave_rows` rows per full cycle), so the wave moves smoothly regardless of block height.
/// `tick` is the frame counter, `speed` is radians per tick (e.g. 0.15); returns brightness in [0.0, 1.0].
pub fn wave_brightness(tick: u64, row: u16, wave_rows: u16, speed: f32) -> f32 {
    use std::f32::consts::PI;

    let rows_per_wave = wave_rows.max(1) as f32;
    let phase = (row as f32 / rows_per_wave) * 2.0 * PI;

    let t = tick as f32 * speed;

    // sin²(t + phase) gives smooth 0-1 oscillation
    let sin_val = (t + phase).sin();
    sin_val * sin_val
}

/// Compute a pulsing brightness in [0.0, 1.0] for a single element (icon, indicator); everything sharing the same tick pulses in unison.
///
/// `tick` is the frame counter, `speed` is radians per tick, and `sin²` has period π, so one full pulse takes `π / (speed * fps)` seconds.
/// At 30fps, `speed = 0.08` gives about a 1.3s cycle; for a 2.5s cycle pass about `0.042`.
pub fn pulse_brightness(tick: u64, speed: f32) -> f32 {
    let t = tick as f32 * speed;
    let sin_val = t.sin();
    sin_val * sin_val
}

/// Brightness of a "waiting on you" cue: [`pulse_brightness`] lifted onto
/// [`AnimationConstants::waiting_floor`].
///
/// The only place the floor and the range are applied, so the pager's diamonds
/// and a browser's cannot end up with different troughs.
pub fn waiting_brightness(tick: u64) -> f32 {
    ANIMATION.waiting_floor
        + pulse_brightness(tick, ANIMATION.waiting_pulse_speed) * ANIMATION.waiting_range
}
