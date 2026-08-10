//! Model-facing context contributed by `session_start` hooks.
//!
//! `ctx.log` is a hook's own log channel and the model never reads it, so a
//! hook that computes something the agent needs at session start — a scratch
//! directory, an identity, a policy — returns it as `additionalContext`. The
//! dispatcher aggregates those strings; this module renders them and decides
//! where they land.
//!
//! They land in the `<user_info>` prefix, the synthetic user message at
//! conversation index 1. That is the one piece of session-scoped context the
//! pipeline already re-establishes verbatim at every rebuild — after a
//! compaction and on a model switch — so a per-session path the agent needs
//! all session long does not evaporate halfway through it.

use super::*;

/// Hard cap (in characters) on the rendered `session_start` context block.
///
/// The block rides in the `<user_info>` prefix of every request for the rest of
/// the session, so an unbounded plugin string would be paid for on every turn.
/// Past the cap the text is clipped with `clip_text`'s `… [+N chars]` marker
/// rather than dropped: a truncated path is still a usable hint, a missing one
/// is not, and the marker tells the model it is looking at a fragment.
pub(crate) const SESSION_START_CONTEXT_MAX_CHARS: usize = 4_000;

/// Render the `additionalContext` a `session_start` dispatch collected into the
/// reminder *body* (no wrapper tag — both consumers add their own), or `None`
/// when every hook stayed silent.
///
/// Several hooks may contribute; entries keep dispatch order (config order,
/// then plugin order) and are separated by a blank line. Blank entries are
/// already dropped at the runner boundary; trimming here guards the http path
/// too. The heading names the source so the model can weigh the text — it is a
/// hook's claim about this session, not a user instruction.
pub(crate) fn format_session_start_context(entries: &[String]) -> Option<String> {
    let joined = entries
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.is_empty() {
        return None;
    }
    let clipped = xai_grok_hooks::event::clip_text(&joined, SESSION_START_CONTEXT_MAX_CHARS);
    Some(format!(
        "Context from this session's session_start hooks:\n\n{clipped}"
    ))
}

impl SessionActor {
    /// Store what this session's `session_start` hooks contributed, so every
    /// later `<user_info>` prefix build can re-establish it. Returns the body
    /// when there was any, so the caller can also surface it right away.
    pub(super) fn record_session_start_context(&self, entries: &[String]) -> Option<String> {
        let body = format_session_start_context(entries)?;
        tracing::info!(
            session_id = %self.session_info.id.0,
            entries = entries.len(),
            body_chars = body.chars().count(),
            "session_start hooks contributed model-facing context"
        );
        *self.session_start_context.borrow_mut() = Some(body.clone());
        Some(body)
    }

    /// Append the recorded `session_start` context to a freshly built
    /// `<user_info>` prefix, wrapped exactly like [`Self::push_system_reminder`]
    /// (same tag, same closing-tag escaping, so hook text cannot break out).
    ///
    /// Every site that builds a prefix calls this, and only those sites, so the
    /// block lands exactly once per prefix: the background build armed at
    /// `Initialize` can finish before the hooks have replied, so appending
    /// inside `build_user_message_prefix` would race, and appending at more
    /// than one consumption point would duplicate.
    pub(super) fn with_session_start_context(&self, prefix: String) -> String {
        let Some(body) = self.session_start_context.borrow().clone() else {
            return prefix;
        };
        let tag = self.reminder_wrapper_tag();
        let body = body.replace(&format!("</{tag}>"), &format!("<\\/{tag}>"));
        format!("{prefix}\n\n<{tag}>\n{body}\n</{tag}>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_hooks_render_nothing() {
        assert!(format_session_start_context(&[]).is_none());
        assert!(format_session_start_context(&["  \n".to_string()]).is_none());
    }

    #[test]
    fn several_hooks_are_joined_in_dispatch_order() {
        let body = format_session_start_context(&["first".into(), "second".into()])
            .expect("non-empty entries render a body");
        assert!(body.contains("first\n\nsecond"));
    }

    #[test]
    fn oversized_context_is_clipped_not_dropped() {
        let body =
            format_session_start_context(&["x".repeat(SESSION_START_CONTEXT_MAX_CHARS + 50)])
                .expect("oversized entries still render");
        assert!(body.contains("… [+50 chars]"));
        assert!(body.chars().count() < SESSION_START_CONTEXT_MAX_CHARS + 200);
    }
}
