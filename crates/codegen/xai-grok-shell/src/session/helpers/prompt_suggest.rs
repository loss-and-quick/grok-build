//! Helpers for next-prompt suggestions.

use crate::config::PromptSuggestModelPin;
use crate::sampling::ConversationItem;
use crate::session::helpers::chat::floor_char_boundary;
use xai_grok_sampling_types::ReasoningEffort;

/// Model used for suggestion calls when nothing pins one (no env /
/// `[models] prompt_suggestion` / remote setting / client hint — see
/// [`effective_suggest_model`]). Suggestion requests must stay on a small,
/// fast model: falling back to the session model would multiply the per-turn
/// cost of the feature and add reasoning-model latency for a throwaway
/// prediction.
pub(crate) const DEFAULT_SUGGEST_MODEL: &str = "grok-build-0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SuggestReasoning {
    pub(crate) effort: Option<ReasoningEffort>,
    pub(crate) reserve_budget: bool,
}

/// Unset effort becomes low only on a reasoning model that is not the
/// non-reasoning alias. That alias stays off even if the catalog lists effort.
/// An explicit value is kept.
pub(crate) fn suggest_request_effort(
    configured: Option<ReasoningEffort>,
    model: &str,
    model_supports_reasoning: bool,
) -> Option<ReasoningEffort> {
    let alias = crate::util::config::NON_REASONING_PROMPT_SUGGEST_MODEL;
    match configured {
        Some(effort) => Some(effort),
        None if model_supports_reasoning && model != alias => Some(ReasoningEffort::Low),
        None => None,
    }
}

pub(crate) fn resolve_suggest_reasoning(
    configured: Option<ReasoningEffort>,
    model: &str,
    supports_reasoning_effort: bool,
    supports_none: bool,
) -> SuggestReasoning {
    if let Some(effort) = configured
        && !matches!(effort, ReasoningEffort::None)
    {
        return SuggestReasoning {
            effort: supports_reasoning_effort.then_some(effort),
            reserve_budget: supports_reasoning_effort,
        };
    }

    if model != crate::util::config::NON_REASONING_PROMPT_SUGGEST_MODEL && supports_reasoning_effort
    {
        return SuggestReasoning {
            effort: supports_none.then_some(ReasoningEffort::None),
            reserve_budget: !supports_none,
        };
    }

    SuggestReasoning {
        effort: None,
        reserve_budget: false,
    }
}

/// Resolve the model for one suggestion request, or `None` to skip the
/// request entirely (controlled disable).
///
/// Precedence: env pin > config.toml/remote pin > client hint (the request's
/// `model` param) > [`DEFAULT_SUGGEST_MODEL`]. Every tier except the env pin
/// is catalog-guarded via `catalog_key`: [`DEFAULT_SUGGEST_MODEL`]
/// (`grok-build-0.1`) is API-key-only and excluded from OAuth catalogs, so
/// firing it (or any unavailable pin) would send a doomed per-turn request
/// that can never render ghost text. Skipping keeps the per-turn cost at
/// zero; deliberately NOT a session-model fallback — a per-turn background
/// call must stay on a small cheap model. The env pin bypasses the guard so
/// `GROK_PROMPT_SUGGESTIONS_MODEL` keeps working for models the catalog does
/// not list (mirrors the pager, which forwards the env value unchecked).
///
/// The value returned is a catalog **key**, not a routing slug, and the caller
/// hands it to the aux sampler resolver, which turns it into an endpoint, a
/// credential and the wire `model` in one step. Returning a slug would be
/// lossy: a `[[provider]]` entry is keyed `<provider>/<model>` and serves the
/// bare `<model>`, so two providers serving one slug produce two entries that
/// only the key tells apart. A pin narrowed to its slug re-resolves to whichever
/// of them was declared last, which sends the request to a provider the operator
/// did not name, on that provider's credential.
///
/// This deliberately replaced a presence predicate (`model_in_catalog`). A
/// presence check cannot catch that class of bug: a provider-qualified pin IS
/// in the catalog, which is precisely why the request gets made at all, and
/// forwarding it as a `model` value is still a 404 by name. A guard on presence
/// is not a guard on correctness — but the answer is to keep the key and resolve
/// it once, not to trade it for a slug.
pub(crate) fn effective_suggest_model(
    pin: &PromptSuggestModelPin,
    client_hint: Option<&str>,
    catalog_key: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let client_hint = client_hint.map(str::trim).filter(|s| !s.is_empty());
    let (model, catalog_guarded) = match pin {
        PromptSuggestModelPin::Env(m) => (m.as_str(), false),
        PromptSuggestModelPin::Pinned(m) => (m.as_str(), true),
        PromptSuggestModelPin::Unpinned => (client_hint.unwrap_or(DEFAULT_SUGGEST_MODEL), true),
    };
    match catalog_key(model) {
        Some(key) => Some(key),
        // Guarded tiers skip rather than fire a request the catalog cannot back.
        None if catalog_guarded => None,
        // Env pin: the explicit escape hatch for an id the catalog does not
        // list, so there is no entry to name — forward it verbatim and let the
        // resolver's first-party fallback tier deal with it.
        None => Some(model.to_owned()),
    }
}

/// Total character budget for the compact transcript (~6k tokens at the bytes/4 estimate).
/// It keeps the per-turn cost of the feature trivial even on long sessions.
const TRANSCRIPT_BUDGET_CHARS: usize = 24_000;

/// Per-message character cap inside the transcript.
/// Long messages (pasted logs, big diffs) carry little signal for next-prompt prediction.
const MESSAGE_CAP_CHARS: usize = 1_500;

/// The model sees a compact transcript and must reply with ONLY the predicted next user message (or nothing).
pub(crate) const SUGGEST_PROMPT_SYSTEM: &str = "You predict the next line the USER will type into their coding agent.\n\
    You see a transcript. The last line is from the agent.\n\
    Write only that next user line, or NONE.\n\n\
    Predict what they would type, not what you think they should do.\n\
    A wrong line is worse than NONE.\n\
    Write NONE if the next line is long, new, or not obvious.\n\
    Write NONE after an error or a misunderstanding.\n\n\
    Never write a line the user already sent.\n\
    Never write filler, a question, or agent voice.\n\
    Never write a new idea they did not ask for.\n\n\
    If you write a line, use 2-12 words in their style.\n\
    Reply with only the line or NONE.";

pub(crate) fn suggestion_size(s: &str) -> (usize, usize) {
    (s.chars().count(), s.split_whitespace().count())
}

/// One transcript line: role label and flattened text content.
fn transcript_line(role: &str, text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut text = text;
    if text.len() > MESSAGE_CAP_CHARS {
        let cut = floor_char_boundary(text, MESSAGE_CAP_CHARS);
        text = &text[..cut];
    }
    Some(format!("{role}: {text}"))
}

/// Keeps genuine `User` messages (skipping runtime-synthesized ones) and `Assistant` text, newest-last.
/// Walks backwards until the character budget is exhausted.
/// Tool calls/results, reasoning, and the system prompt are dropped.
/// The user/assistant dialogue carries the signal for "what will the user type next", and dropping the rest keeps the request cheap.
///
/// Returns `None` when the conversation has no assistant reply yet (nothing to predict from).
pub(crate) fn build_transcript(conversation: &[ConversationItem]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut saw_assistant = false;

    for item in conversation.iter().rev() {
        let line = match item {
            ConversationItem::User(u) => {
                if u.synthetic_reason.is_some() {
                    continue;
                }
                transcript_line("User", &item.text_content())
            }
            ConversationItem::Assistant(_) => {
                let line = transcript_line("Agent", &item.text_content());
                if line.is_some() {
                    saw_assistant = true;
                }
                line
            }
            _ => continue,
        };
        let Some(line) = line else { continue };
        if used + line.len() > TRANSCRIPT_BUDGET_CHARS && !lines.is_empty() {
            break;
        }
        used += line.len();
        lines.push(line);
    }

    if !saw_assistant || lines.is_empty() {
        return None;
    }

    lines.reverse();
    Some(lines.join("\n\n"))
}

pub(crate) fn suggest_prompt_user_message(transcript: &str, cwd: &str) -> String {
    format!(
        "CWD: {cwd}\n\nTranscript:\n\n{transcript}\n\n\
         Predict the user's next message. Reply with ONLY the suggestion text."
    )
}

/// Returns `None` when there is nothing to show. Matches the eval: empty or a
/// silence token is NONE. Other text is shown as the first line.
pub(crate) fn sanitize_suggestion(raw: &str) -> Option<String> {
    let line = raw.trim().lines().next()?.trim();
    let line = line
        .trim_start_matches(['"', '\'', '`', '“', '‘'])
        .trim_end_matches(['"', '\'', '`', '”', '’'])
        .trim();

    if line.is_empty() {
        return None;
    }

    let lowered = line.to_ascii_lowercase();
    let meta = [
        "none",
        "n/a",
        "no suggestion",
        "nothing",
        "(silence)",
        "silence",
        "null",
    ];
    if meta
        .iter()
        .any(|m| lowered == *m || lowered.starts_with(&format!("{m}.")))
    {
        return None;
    }

    Some(line.to_owned())
}

/// Minimum word count for the deterministic repeat filter. Short
/// command-like replies ("yes", "run tests", "try again") legitimately
/// recur across a session; a repeated multi-word task prompt is the
/// "it suggested my old prompt back to me" failure mode.
const REPEAT_MIN_WORDS: usize = 4;

/// Case- and whitespace-insensitive form used for repeat comparison, with
/// trailing sentence punctuation dropped.
fn normalize_for_repeat(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase()
}

/// Whether a sanitized suggestion merely repeats a message the user already
/// sent. Deterministic backstop behind the system prompt's anti-repeat rule:
/// prompt guidance reduces repeats, this guarantees an exact (normalized)
/// re-suggestion of a past multi-word prompt never renders as ghost text.
/// Short suggestions (< [`REPEAT_MIN_WORDS`] words) are exempt — repeating
/// "yes" or "run tests" is often exactly what the user is about to type.
pub(crate) fn is_repeat_of_user_message(
    suggestion: &str,
    conversation: &[ConversationItem],
) -> bool {
    if suggestion.split_whitespace().count() < REPEAT_MIN_WORDS {
        return false;
    }
    let needle = normalize_for_repeat(suggestion);
    conversation.iter().any(|item| match item {
        ConversationItem::User(u) if u.synthetic_reason.is_none() => {
            normalize_for_repeat(&item.text_content()) == needle
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PromptSuggestModelPin as Pin;

    // -- effective_suggest_model ---------------------------------------------

    /// Catalog stub for entries whose key is already the routing slug (the
    /// common shape: a plain `[model.*]` table or a remote catalog entry).
    fn catalog_of(ids: &'static [&'static str]) -> impl Fn(&str) -> Option<String> {
        move |m: &str| ids.contains(&m).then(|| m.to_owned())
    }

    /// Catalog stub that lists nothing.
    fn empty_catalog(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn effective_model_default_requires_catalog() {
        // No pin, no hint: the built-in default fires only when this shell's
        // catalog can sample it.
        assert_eq!(
            effective_suggest_model(&Pin::Unpinned, None, catalog_of(&[DEFAULT_SUGGEST_MODEL]))
                .as_deref(),
            Some(DEFAULT_SUGGEST_MODEL)
        );
        // OAuth catalogs exclude grok-build-0.1 → skip the request entirely,
        // never a doomed call (and never the session model).
        assert_eq!(
            effective_suggest_model(&Pin::Unpinned, None, empty_catalog),
            None
        );
    }

    #[test]
    fn effective_model_client_hint_beats_default_and_is_guarded() {
        assert_eq!(
            effective_suggest_model(&Pin::Unpinned, Some("hinted"), catalog_of(&["hinted"]))
                .as_deref(),
            Some("hinted")
        );
        // A hint the shell can't sample skips — no silent fall-through.
        assert_eq!(
            effective_suggest_model(&Pin::Unpinned, Some("hinted"), empty_catalog),
            None
        );
        // Blank hints are ignored: the default tier applies.
        assert_eq!(
            effective_suggest_model(
                &Pin::Unpinned,
                Some("  "),
                catalog_of(&[DEFAULT_SUGGEST_MODEL])
            )
            .as_deref(),
            Some(DEFAULT_SUGGEST_MODEL)
        );
    }

    #[test]
    fn effective_model_pin_beats_client_hint_and_is_guarded() {
        assert_eq!(
            effective_suggest_model(
                &Pin::Pinned("pinned".into()),
                Some("hinted"),
                catalog_of(&["pinned", "hinted"])
            )
            .as_deref(),
            Some("pinned")
        );
        // A pinned-but-unavailable model skips — the pin is an explicit
        // choice, not a preference list; no fall-through to hint or default.
        assert_eq!(
            effective_suggest_model(
                &Pin::Pinned("pinned".into()),
                Some("hinted"),
                catalog_of(&["hinted"])
            ),
            None
        );
    }

    #[test]
    fn effective_model_env_pin_bypasses_catalog_guard() {
        // GROK_PROMPT_SUGGESTIONS_MODEL is the explicit escape hatch: used
        // verbatim even when the catalog does not list the model (mirrors
        // the pager, which forwards the env value unchecked).
        assert_eq!(
            effective_suggest_model(
                &Pin::Env("custom-model".into()),
                Some("hinted"),
                empty_catalog
            )
            .as_deref(),
            Some("custom-model")
        );
    }

    #[test]
    fn configured_reasoning_reserves_budget() {
        assert_eq!(
            resolve_suggest_reasoning(Some(ReasoningEffort::Low), "session", true, true),
            SuggestReasoning {
                effort: Some(ReasoningEffort::Low),
                reserve_budget: true,
            }
        );
    }

    #[test]
    fn reasoning_off_uses_none_when_the_fallback_supports_it() {
        assert_eq!(
            resolve_suggest_reasoning(None, "session", true, true),
            SuggestReasoning {
                effort: Some(ReasoningEffort::None),
                reserve_budget: false,
            }
        );
    }

    #[test]
    fn reasoning_off_reserves_budget_when_the_fallback_has_no_none_effort() {
        assert_eq!(
            resolve_suggest_reasoning(None, "session", true, false),
            SuggestReasoning {
                effort: None,
                reserve_budget: true,
            }
        );
    }

    #[test]
    fn alias_keeps_the_small_non_reasoning_budget() {
        assert_eq!(
            resolve_suggest_reasoning(
                None,
                crate::util::config::NON_REASONING_PROMPT_SUGGEST_MODEL,
                true,
                false,
            ),
            SuggestReasoning {
                effort: None,
                reserve_budget: false,
            }
        );
    }

    /// Every spelling of a pin comes out as the one catalog key that names its
    /// entry — including the provider-qualified form `[[provider]]` expansion
    /// mints. Narrowing that to the bare slug would throw away the only thing
    /// distinguishing two providers that serve it.
    #[test]
    fn effective_model_resolves_every_spelling_of_a_pin_to_its_catalog_key() {
        let catalog_key = |m: &str| match m {
            "acme/some-model" | "some-model" => Some("acme/some-model".to_owned()),
            _ => None,
        };
        for spelling in ["acme/some-model", "some-model"] {
            assert_eq!(
                effective_suggest_model(&Pin::Pinned(spelling.into()), None, catalog_key)
                    .as_deref(),
                Some("acme/some-model"),
                "pin {spelling:?} must resolve to the catalog key, not to a slug"
            );
        }
        // A client hint is resolved the same way...
        assert_eq!(
            effective_suggest_model(&Pin::Unpinned, Some("some-model"), catalog_key).as_deref(),
            Some("acme/some-model")
        );
        // ...and so is an env pin the catalog happens to know: bypassing the
        // guard is not licence to route by an ambiguous name.
        assert_eq!(
            effective_suggest_model(&Pin::Env("some-model".into()), None, catalog_key).as_deref(),
            Some("acme/some-model")
        );
    }

    // -- sanitize_suggestion ------------------------------------------------

    #[test]
    fn sanitize_accepts_short_imperative() {
        assert_eq!(
            sanitize_suggestion("run the tests").as_deref(),
            Some("run the tests")
        );
    }

    #[test]
    fn sanitize_strips_quotes_and_backticks() {
        assert_eq!(
            sanitize_suggestion("\"commit this\"").as_deref(),
            Some("commit this")
        );
        assert_eq!(sanitize_suggestion("`push it`").as_deref(), Some("push it"));
    }

    #[test]
    fn sanitize_takes_first_line_only() {
        assert_eq!(
            sanitize_suggestion("run the tests\nthen commit").as_deref(),
            Some("run the tests")
        );
    }

    #[test]
    fn sanitize_rejects_none_and_meta() {
        for s in ["NONE", "none", "n/a", "no suggestion", "(silence)", ""] {
            assert_eq!(sanitize_suggestion(s), None, "should reject {s:?}");
        }
    }

    #[test]
    fn unset_effort_is_low_only_on_a_reasoning_model() {
        assert_eq!(
            suggest_request_effort(None, "grok-4.6", true),
            Some(ReasoningEffort::Low)
        );
        assert_eq!(suggest_request_effort(None, "grok-4.6", false), None);
        assert_eq!(
            suggest_request_effort(
                None,
                crate::util::config::NON_REASONING_PROMPT_SUGGEST_MODEL,
                true
            ),
            None
        );
        assert_eq!(
            suggest_request_effort(Some(ReasoningEffort::High), "grok-4.6", true),
            Some(ReasoningEffort::High)
        );
        assert_eq!(
            suggest_request_effort(Some(ReasoningEffort::None), "grok-4.6", true),
            Some(ReasoningEffort::None)
        );
    }

    // -- build_transcript ---------------------------------------------------

    fn user(text: &str) -> ConversationItem {
        ConversationItem::user(text.to_owned())
    }

    fn assistant(text: &str) -> ConversationItem {
        ConversationItem::assistant(text.to_owned())
    }

    // -- is_repeat_of_user_message -------------------------------------------

    #[test]
    fn repeat_filter_rejects_verbatim_past_prompt() {
        let conv = vec![user("fix the flaky auth test"), assistant("Fixed it")];
        assert!(is_repeat_of_user_message("fix the flaky auth test", &conv));
    }

    #[test]
    fn repeat_filter_is_case_whitespace_and_punctuation_insensitive() {
        let conv = vec![user("Fix  the flaky\nauth test."), assistant("Fixed it")];
        assert!(is_repeat_of_user_message("fix the flaky auth test!", &conv));
    }

    #[test]
    fn repeat_filter_exempts_short_suggestions() {
        let conv = vec![user("run the tests"), assistant("3 failures")];
        // 3 words — legitimately recurs after new changes.
        assert!(!is_repeat_of_user_message("run the tests", &conv));
        assert!(!is_repeat_of_user_message("yes", &conv));
    }

    #[test]
    fn repeat_filter_allows_novel_suggestions() {
        let conv = vec![user("fix the flaky auth test"), assistant("Fixed it")];
        assert!(!is_repeat_of_user_message("commit and push the fix", &conv));
    }

    #[test]
    fn repeat_filter_ignores_synthetic_user_messages() {
        let mut synthetic = user("please review the changes now");
        if let ConversationItem::User(u) = &mut synthetic {
            u.synthetic_reason = Some(crate::sampling::SyntheticReason::SystemReminder);
        }
        let conv = vec![synthetic, assistant("done")];
        assert!(!is_repeat_of_user_message(
            "please review the changes now",
            &conv
        ));
    }

    #[test]
    fn transcript_keeps_user_and_assistant_in_order() {
        let conv = vec![
            ConversationItem::system("sys".to_owned()),
            user("fix the bug"),
            assistant("Fixed it in foo.rs"),
        ];
        let t = build_transcript(&conv).unwrap();
        assert_eq!(t, "User: fix the bug\n\nAgent: Fixed it in foo.rs");
    }

    #[test]
    fn transcript_requires_an_assistant_reply() {
        let conv = vec![ConversationItem::system("sys".to_owned()), user("hello")];
        assert!(build_transcript(&conv).is_none());
        assert!(build_transcript(&[]).is_none());
    }

    #[test]
    fn transcript_skips_synthetic_user_messages() {
        let mut synthetic = ConversationItem::user("synthetic reminder".to_owned());
        if let ConversationItem::User(u) = &mut synthetic {
            u.synthetic_reason = Some(crate::sampling::SyntheticReason::SystemReminder);
        }
        let conv = vec![user("real question"), synthetic, assistant("answer")];
        let t = build_transcript(&conv).unwrap();
        assert!(!t.contains("synthetic reminder"));
        assert!(t.contains("User: real question"));
    }

    #[test]
    fn transcript_caps_long_messages() {
        let long = "a".repeat(10_000);
        let conv = vec![user(&long), assistant("ok")];
        let t = build_transcript(&conv).unwrap();
        assert!(
            t.len() < 2_000,
            "long message must be truncated: {}",
            t.len()
        );
    }

    #[test]
    fn transcript_budget_keeps_newest_messages() {
        let filler = "b".repeat(MESSAGE_CAP_CHARS);
        let mut conv = Vec::new();
        for _ in 0..40 {
            conv.push(user(&filler));
            conv.push(assistant(&filler));
        }
        conv.push(user("newest question"));
        conv.push(assistant("newest answer"));
        let t = build_transcript(&conv).unwrap();
        assert!(t.len() <= TRANSCRIPT_BUDGET_CHARS + MESSAGE_CAP_CHARS + 64);
        assert!(t.contains("newest question"));
        assert!(t.ends_with("Agent: newest answer"));
    }
}
