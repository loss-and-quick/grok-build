//! The click-to-copy affordance, shared by the native sign-in screen and by
//! plugin panels.
//!
//! Both surfaces show a URL the user cannot select with the mouse (the TUI owns
//! mouse capture) and both answer it the same way: one "click `here` to copy."
//! line plus a fixed feedback slot underneath. The wording, the styling of the
//! clickable word, and the three delivery strings live here so the two surfaces
//! cannot teach the user two different gestures.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::clipboard::ClipboardDelivery;
use crate::theme::Theme;

/// The clickable word inside the copy prompt.
pub(crate) const COPY_LINK_HERE: &str = "here";

/// Text following the clickable word.
pub(crate) const COPY_LINK_SUFFIX: &str = " to copy.";

/// `<prefix>here to copy.`, with `here` underlined in the accent colour.
///
/// Not aligned — callers set the alignment their layout needs (the welcome
/// screen centers it, a plugin panel keeps it flush-left).
pub(crate) fn copy_link_line(theme: &Theme, prefix: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(theme.gray_bright)),
        Span::styled(
            COPY_LINK_HERE,
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::UNDERLINED),
        ),
        Span::styled(COPY_LINK_SUFFIX, Style::default().fg(theme.gray_bright)),
    ])
}

/// Character length of the text [`copy_link_line`] paints for `prefix`. Used
/// for wrap-row math and for the click hit-rect width.
pub(crate) fn copy_link_len(prefix: &str) -> usize {
    prefix.len() + COPY_LINK_HERE.len() + COPY_LINK_SUFFIX.len()
}

/// Confirm-on-copy wording for `delivery`. The single source for these three
/// strings.
pub(crate) fn copy_feedback_text(delivery: ClipboardDelivery) -> &'static str {
    match delivery {
        ClipboardDelivery::Confirmed => "copied!",
        ClipboardDelivery::Unverified => "copy sent\u{2014}verify paste",
        ClipboardDelivery::Failed => "copy failed",
    }
}

/// The feedback slot under a copy prompt. Always one line so the layout does
/// not jump when feedback appears; empty until a copy has been attempted.
pub(crate) fn copy_feedback_line(
    theme: &Theme,
    delivery: Option<ClipboardDelivery>,
) -> Line<'static> {
    match delivery {
        Some(delivery) => Line::from(Span::styled(
            copy_feedback_text(delivery),
            Style::default().fg(theme.gray),
        )),
        None => Line::default(),
    }
}
