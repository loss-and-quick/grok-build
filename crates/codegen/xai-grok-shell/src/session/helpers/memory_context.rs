//! Format memory search results as `<system-reminder>` content.
//!
//! Used for:
//! - Session start: inject relevant past context on the first turn
//! - Post-compaction: recover relevant memory after context is lost

use xai_chat_state::{MEMORY_CONTEXT_CLOSE_TAG, MEMORY_CONTEXT_OPEN_TAG};
use xai_grok_sampling_types::ConversationItem;
use xai_grok_tools::types::memory_backend::{MemorySearchResult, format_staleness_note};

const SNIPPET_MAX_CHARS: usize = 500;

/// Returns `true` if a memory-context block is already persisted in the leading system message.
/// Callers reuse a persisted block verbatim instead of re-searching.
/// A re-scored block would mutate the system-prompt prefix and bust the KV cache for the whole downstream conversation.
pub fn conversation_has_memory_context(items: &[ConversationItem]) -> bool {
    matches!(
        items.first(),
        Some(ConversationItem::System(sys)) if sys.content.contains(MEMORY_CONTEXT_OPEN_TAG)
    )
}

/// Read the injected `<memory-context>` block back out of the leading system
/// message, with the number of results it carries.
///
/// Recovered from where it lives rather than re-searched, for the same reason
/// [`conversation_has_memory_context`] exists: the search is one-shot and a
/// second one would score differently, so a re-render would show `/context` a
/// block the model was never sent. What is returned is the text the request
/// actually carries.
///
/// Returns `None` for a non-system item, for a system message with no block,
/// and for a block whose closing tag is missing — a half-block would be
/// measured short and displayed truncated.
pub fn injected_memory_context(item: &ConversationItem) -> Option<(String, usize)> {
    let ConversationItem::System(sys) = item else {
        return None;
    };
    let start = sys.content.find(MEMORY_CONTEXT_OPEN_TAG)?;
    let rest = &sys.content[start..];
    let end = rest.find(MEMORY_CONTEXT_CLOSE_TAG)? + MEMORY_CONTEXT_CLOSE_TAG.len();
    let block = &rest[..end];
    // Counting the headings beats threading a count through the system
    // message, which stores text and nothing else.
    Some((block.to_string(), count_result_headings(block)))
}

/// Count the result headings [`format_memory_reminder`] wrote into a block.
///
/// Counting every `### Result ` occurrence would count the snippets too, and
/// each snippet is a verbatim excerpt of the user's memory markdown. Memory
/// records past sessions, so a stored `/context` output or search transcript
/// carries exactly those lines and would inflate the total.
///
/// Two things narrow it to what the formatter writes: a heading is a whole
/// line that also carries the `(score:` it emits, and the headings are
/// numbered from one, so only the next number in sequence counts. Tracking the
/// fences instead would be worse — a snippet may hold fences of its own, and
/// the parity would desync and silently drop real headings.
fn count_result_headings(block: &str) -> usize {
    let mut seen = 0usize;
    for line in block.lines() {
        let Some(rest) = line.strip_prefix("### Result ") else {
            continue;
        };
        if rest.starts_with(&format!("{} (score:", seen + 1)) {
            seen += 1;
        }
    }
    seen
}

/// Format memory search results as a markdown section for system-reminder injection.
///
/// Each result is formatted with score, source, file path, line range, and the snippet in a fenced code block (preserving newlines/markdown).
/// This matches the output format of the `memory_search` tool.
///
/// Returns `None` if results are empty.
pub fn format_memory_reminder(results: &[MemorySearchResult]) -> Option<String> {
    if results.is_empty() {
        return None;
    }

    let mut section = format!(
        "{MEMORY_CONTEXT_OPEN_TAG}\n## Relevant Memory from Past Sessions\n\n\
         Treat memory as historical context, not automatically as the current plan. \
         Verify recalled paths, commands, \
         repository state, and external facts with live tools before relying on them; \
         prefer current evidence when it conflicts with memory.\n\n"
    );

    for (i, r) in results.iter().enumerate() {
        let truncated = r.snippet.chars().count() > SNIPPET_MAX_CHARS;
        let mut snippet: String = r.snippet.chars().take(SNIPPET_MAX_CHARS).collect();
        if truncated {
            snippet.push_str("...");
        }
        let staleness = format_staleness_note(&r.source, r.created_at);
        section.push_str(&format!(
            "### Result {} (score: {:.2}, source: {})\n\
             **File:** {} (lines {}-{})\n\
             {}```\n{}\n```\n\n",
            i + 1,
            r.score,
            r.source,
            r.path,
            r.start_line,
            r.end_line,
            staleness,
            snippet,
        ));
    }

    section.push_str(MEMORY_CONTEXT_CLOSE_TAG);
    Some(section)
}

/// Check if a message looks like a greeting or generic opener.
///
/// Used to detect vague first messages that won't produce useful memory search results, so we can fall back to a broader project-context query.
pub fn is_greeting(text: &str) -> bool {
    const GREETINGS: &[&str] = &[
        "hi",
        "hey",
        "hello",
        "howdy",
        "continue",
        "start",
        "begin",
        "go",
        "good morning",
        "good afternoon",
        "good evening",
        "what's up",
        "whats up",
        "sup",
    ];
    let lowered = text.to_lowercase();
    let trimmed = lowered.trim().trim_end_matches(['.', '!', '?', ',']);
    GREETINGS.contains(&trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_empty() {
        assert_eq!(format_memory_reminder(&[]), None);
    }

    #[test]
    fn test_format_single_result() {
        let results = vec![MemorySearchResult {
            chunk_id: "test:0".to_string(),
            path: "MEMORY.md".to_string(),
            start_line: 0,
            end_line: 5,
            score: 0.9,
            snippet: "Use tracing for logging, never println!".to_string(),
            source: "workspace".to_string(),
            created_at: None,
        }];
        let output = format_memory_reminder(&results).unwrap();
        assert!(output.contains("<memory-context>"));
        assert!(output.contains("### Result 1"));
        assert!(output.contains("score: 0.90"));
        assert!(output.contains("**File:** MEMORY.md (lines 0-5)"));
        assert!(output.contains("```\nUse tracing for logging"));
    }

    #[test]
    fn test_format_preserves_newlines() {
        let results = vec![MemorySearchResult {
            chunk_id: "test:0".to_string(),
            path: "MEMORY.md".to_string(),
            start_line: 0,
            end_line: 3,
            score: 0.85,
            snippet: "## Conventions\n\n- Use Rust\n- No clones".to_string(),
            source: "workspace".to_string(),
            created_at: None,
        }];
        let output = format_memory_reminder(&results).unwrap();
        assert!(
            output.contains("## Conventions\n\n- Use Rust\n- No clones"),
            "newlines in snippet should be preserved, not collapsed"
        );
    }

    #[test]
    fn test_format_truncates_long_snippets() {
        let results = vec![MemorySearchResult {
            chunk_id: "test:0".to_string(),
            path: "test.md".to_string(),
            start_line: 0,
            end_line: 5,
            score: 0.8,
            snippet: "x".repeat(1000),
            source: "session".to_string(),
            created_at: None,
        }];
        let output = format_memory_reminder(&results).unwrap();
        // The snippet is truncated to SNIPPET_MAX_CHARS (500) with a "..." suffix
        assert!(!output.contains(&"x".repeat(501)));
        assert!(output.contains(&format!("{}...", "x".repeat(500))));
    }

    #[test]
    fn test_format_multiple_results() {
        let results = vec![
            MemorySearchResult {
                chunk_id: "a:0".to_string(),
                path: "MEMORY.md".to_string(),
                start_line: 0,
                end_line: 5,
                score: 0.9,
                snippet: "First result".to_string(),
                source: "workspace".to_string(),
                created_at: None,
            },
            MemorySearchResult {
                chunk_id: "b:0".to_string(),
                path: "session.md".to_string(),
                start_line: 10,
                end_line: 15,
                score: 0.7,
                snippet: "Second result".to_string(),
                source: "session".to_string(),
                created_at: None,
            },
        ];
        let output = format_memory_reminder(&results).unwrap();
        assert!(output.contains("### Result 1"));
        assert!(output.contains("### Result 2"));
        assert!(output.contains("score: 0.90"));
        assert!(output.contains("score: 0.70"));
    }

    // -----------------------------------------------------------------------
    // conversation_has_memory_context (idempotency guard) tests
    // -----------------------------------------------------------------------

    fn sample_result() -> MemorySearchResult {
        MemorySearchResult {
            chunk_id: "test:0".into(),
            path: "MEMORY.md".into(),
            start_line: 0,
            end_line: 5,
            score: 0.9,
            snippet: "Project uses Rust for backend services.".into(),
            source: "workspace".into(),
            created_at: None,
        }
    }

    #[test]
    fn test_detects_persisted_block_in_system_message() {
        let block = format_memory_reminder(&[sample_result()]).unwrap();
        let system_content = format!("You are a helpful assistant.\n\n{block}");
        let conversation = vec![
            ConversationItem::system(system_content),
            ConversationItem::user("help me fix the auth bug"),
        ];
        assert!(
            conversation_has_memory_context(&conversation),
            "an already-injected memory-context block must be detected so it is reused, not re-searched"
        );
    }

    #[test]
    fn test_no_block_when_system_lacks_marker() {
        let conversation = vec![
            ConversationItem::system("You are a helpful assistant."),
            ConversationItem::user("hi"),
        ];
        assert!(!conversation_has_memory_context(&conversation));
    }

    #[test]
    fn test_no_block_when_no_leading_system_message() {
        let conversation = vec![ConversationItem::user("hi")];
        assert!(!conversation_has_memory_context(&conversation));
    }

    #[test]
    fn test_no_block_for_empty_conversation() {
        assert!(!conversation_has_memory_context(&[]));
    }

    // -----------------------------------------------------------------------
    // staleness annotation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_staleness_shown_for_old_session_result() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let results = vec![MemorySearchResult {
            chunk_id: "s:0".into(),
            path: "session.md".into(),
            start_line: 0,
            end_line: 5,
            score: 0.8,
            snippet: "old info".into(),
            source: "session".into(),
            created_at: Some(now - 86400 * 10),
        }];
        let output = format_memory_reminder(&results).unwrap();
        assert!(
            output.contains("**Stale ("),
            "10-day-old session result should show stale warning, got: {output}"
        );
    }

    #[test]
    fn test_no_staleness_for_workspace_result() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let results = vec![MemorySearchResult {
            chunk_id: "w:0".into(),
            path: "MEMORY.md".into(),
            start_line: 0,
            end_line: 5,
            score: 0.9,
            snippet: "workspace data".into(),
            source: "workspace".into(),
            created_at: Some(now - 86400 * 30),
        }];
        let output = format_memory_reminder(&results).unwrap();
        assert!(
            !output.contains("**Stale (") && !output.contains("**Note ("),
            "workspace result must not show staleness, got: {output}"
        );
    }

    // -----------------------------------------------------------------------
    // is_greeting tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_greeting_detection() {
        assert!(is_greeting("hi"));
        assert!(is_greeting("Hey!"));
        assert!(is_greeting("Hello."));
        assert!(is_greeting("good morning"));
        assert!(is_greeting("continue"));
        assert!(is_greeting("  HELLO  "));
    }

    #[test]
    fn test_non_greeting() {
        assert!(!is_greeting("help me fix the auth bug"));
        assert!(!is_greeting("implement feature X"));
        assert!(!is_greeting("what does this function do"));
        assert!(!is_greeting("hi there, can you help me with something"));
    }

    // -----------------------------------------------------------------------
    // Injection counter semantics tests
    // -----------------------------------------------------------------------

    /// Empty results must return `None`: `memory_injection_count` is only incremented when `memory_reminder.is_some()`.
    #[test]
    fn test_format_memory_reminder_empty_results_is_none() {
        use xai_grok_tools::types::memory_backend::MemorySearchResult;
        let results: Vec<MemorySearchResult> = vec![];
        let reminder = format_memory_reminder(&results);
        assert!(
            reminder.is_none(),
            "empty results must produce None — injection_count must NOT increment"
        );
    }

    // ── reading the injected block back out ───────────────────────────

    fn result(snippet: &str) -> xai_grok_tools::types::memory_backend::MemorySearchResult {
        xai_grok_tools::types::memory_backend::MemorySearchResult {
            chunk_id: "test:0".into(),
            path: "/mem/MEMORY.md".into(),
            start_line: 0,
            end_line: 3,
            score: 0.85,
            snippet: snippet.into(),
            source: "workspace".into(),
            created_at: None,
        }
    }

    #[test]
    fn the_injected_block_is_recovered_whole_with_its_result_count() {
        // The block sits inside a larger system prompt; `/context` must show
        // the block, not the prompt it is embedded in.
        let block = format_memory_reminder(&[result("first"), result("second")])
            .expect("two results render a block");
        let system = ConversationItem::system(format!("You are an agent.\n\n{block}\n\nEnd."));
        let (recovered, results) =
            injected_memory_context(&system).expect("the block is in the system message");
        assert_eq!(recovered, block);
        assert_eq!(results, 2);
    }

    /// A memory entry is the user's own markdown, and memory records past
    /// sessions — so a snippet can hold the text of an earlier injection.
    /// Those lines must not be counted as results of this one.
    #[test]
    fn a_snippet_quoting_an_earlier_injection_does_not_inflate_the_count() {
        let quoted = "### Result 1 (score: 0.99, source: workspace)\n\
                      **File:** MEMORY.md (lines 0-3)\n\
                      ### Result 2 (score: 0.50, source: session)\n\
                      ### Result nine (score: 0.10, source: session)";
        let block = format_memory_reminder(&[result(quoted), result("second")])
            .expect("two results render a block");
        let system = ConversationItem::system(block);
        let (_, results) = injected_memory_context(&system).expect("the block is recovered");
        assert_eq!(
            results, 2,
            "only the headings this block wrote are counted, not the quoted ones"
        );
    }

    #[test]
    fn a_system_message_without_a_block_yields_nothing() {
        let system = ConversationItem::system("You are an agent.");
        assert!(injected_memory_context(&system).is_none());
    }

    #[test]
    fn a_block_missing_its_closing_tag_is_refused() {
        // Half a block would be measured short and displayed truncated; better
        // to show no row than a row that under-reports.
        let system = ConversationItem::system(format!("{MEMORY_CONTEXT_OPEN_TAG}\n## Relevant"));
        assert!(injected_memory_context(&system).is_none());
    }

    #[test]
    fn a_non_system_item_is_not_searched_for_a_block() {
        let block = format_memory_reminder(&[result("first")]).expect("a block");
        assert!(injected_memory_context(&ConversationItem::user(block)).is_none());
    }

    /// Confirms that `memory_injection_count` increments when there are actual results to inject.
    #[test]
    fn test_format_memory_reminder_with_results_is_some() {
        use xai_grok_tools::types::memory_backend::MemorySearchResult;
        let results = vec![MemorySearchResult {
            chunk_id: "test:0".into(),
            path: "/mem/MEMORY.md".into(),
            start_line: 0,
            end_line: 3,
            score: 0.85,
            snippet: "Project uses Rust for backend services.".into(),
            source: "workspace".into(),
            created_at: None,
        }];
        let reminder = format_memory_reminder(&results);
        assert!(
            reminder.is_some(),
            "non-empty results must produce Some(_) — injection_count SHOULD increment"
        );
    }
}
