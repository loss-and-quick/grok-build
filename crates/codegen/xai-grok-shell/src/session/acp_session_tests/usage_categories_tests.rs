//! Actor-level tests for the `/context` usage categories.
//! They cover populated rows with counts, the compat harness suppressing the MCP row, and parity between the MCP snapshot and the injected reminder.
use super::support::*;
use super::*;
use crate::session::tool_index::{ServerMetadata, ToolMetadata};
fn mcp_tool(server: &str, tool: &str) -> ToolMetadata {
    ToolMetadata {
        qualified_name: format!("{server}__{tool}"),
        server_name: server.to_string(),
        tool_name: tool.to_string(),
        description: format!("{tool} description"),
        parameters: vec!["arg".to_string()],
        input_schema: serde_json::json!({"type": "object"}),
    }
}
fn install_mcp_servers(actor: &SessionActor) {
    let mut snapshot = actor.tool_metadata_snapshot.lock().unwrap();
    snapshot.tools = vec![mcp_tool("demo", "echo"), mcp_tool("demo", "add")];
    snapshot.servers = vec![ServerMetadata {
        name: "demo".to_string(),
        description: Some("A demo server.".to_string()),
    }];
    snapshot.mcp_initialized = true;
}
async fn seed_skills(actor: &SessionActor, names: &[&str]) {
    let skills = names
        .iter()
        .map(
            |name| xai_grok_tools::implementations::skills::types::SkillInfo {
                name: name.to_string(),
                description: format!("Does {name} things."),
                path: format!("/skills/{name}/SKILL.md"),
                ..Default::default()
            },
        )
        .collect();
    let bridge = actor.tool_bridge_handle();
    bridge
        .seed_skill_discovery(None, None, skills, None, None, None, Default::default())
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn usage_categories_include_skills_and_mcp_with_counts() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            seed_skills(&actor, &["alpha", "beta"]).await;
            install_mcp_servers(&actor);
            let rows = actor.usage_categories(None).await;
            assert_eq!(rows.len(), 2, "{rows:?}");
            let skills = &rows[0];
            assert_eq!(skills.label, "Skills");
            assert_eq!(skills.detail.as_deref(), Some("2 skills"));
            assert!(skills.tokens > 0);
            let mcp = &rows[1];
            assert_eq!(mcp.label, "MCP servers");
            assert_eq!(mcp.detail.as_deref(), Some("1 server"));
            assert!(mcp.tokens > 0);
            let info = actor.build_session_info().await;
            assert_eq!(info.context.usage_categories.len(), 2);
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn usage_categories_include_project_instructions_with_count() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let def = actor.agent.borrow().definition().clone();
            let bridge = actor.tool_bridge_handle();
            let ctx = xai_grok_agent::PromptContext {
                agents_md_files: vec![
                    xai_grok_agent::prompt::agents_md::AgentConfigFile {
                        file_name: "AGENTS.md".into(),
                        file_path: "/repo/AGENTS.md".into(),
                        content: "# Root\nUse rustfmt.".into(),
                    },
                    xai_grok_agent::prompt::agents_md::AgentConfigFile {
                        file_name: "AGENTS.md".into(),
                        file_path: "/repo/crates/AGENTS.md".into(),
                        content: "# Crate\nPrefer unit tests.".into(),
                    },
                ],
                ..Default::default()
            };
            *actor.agent.borrow_mut() = xai_grok_agent::Agent::new(
                def,
                ctx,
                String::new(),
                bridge,
                xai_grok_agent::ReminderPolicy::default(),
                xai_grok_agent::CompactionPolicy::default(),
                vec![],
                false,
            );
            let rows = actor.usage_categories(None).await;
            let agents = rows
                .iter()
                .find(|row| row.label == "Project instructions")
                .expect("Project instructions row");
            assert_eq!(agents.detail.as_deref(), Some("2 files"));
            assert!(agents.tokens > 0, "{agents:?}");
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn usage_categories_include_workflows_when_enabled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.background_workflows_enabled = true;
            let rows = actor.usage_categories(None).await;
            let workflows = rows
                .iter()
                .find(|row| row.label == "Workflows")
                .expect("workflows row");
            assert!(workflows.tokens > 0, "{workflows:?}");
            assert!(
                workflows
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("workflow")),
                "{workflows:?}"
            );
            let listing = actor.workflow_listing_for_prompt().expect("listing");
            assert!(listing.contains("deep-research"), "{listing}");
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn baseline_reminder_lists_workflows_under_skills() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.background_workflows_enabled = true;
            seed_skills(&actor, &["commit"]).await;
            let mut conversation = vec![ConversationItem::system("sys")];
            actor
                .inject_baseline_skill_reminder(&mut conversation)
                .await;
            let reminder = conversation
                .iter()
                .find_map(|item| {
                    matches!(
                        item,
                        ConversationItem::User(u)
                            if u.synthetic_reason
                                == Some(xai_grok_sampling_types::SyntheticReason::SystemReminder)
                    )
                    .then(|| item.text_content())
                })
                .expect("baseline reminder");
            let commit_at = reminder
                .find("commit")
                .expect("skill name must appear in reminder");
            let workflows_at = reminder
                .find("deep-research")
                .expect("workflow name must appear in reminder");
            assert!(
                commit_at < workflows_at,
                "workflows must sit under skills:\n{reminder}"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn baseline_reminder_lists_workflows_when_there_are_no_skills() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.background_workflows_enabled = true;
            let mut conversation = vec![ConversationItem::system("sys")];
            actor
                .inject_baseline_skill_reminder(&mut conversation)
                .await;
            let reminder = conversation
                .iter()
                .find_map(|item| {
                    matches!(
                        item,
                        ConversationItem::User(u)
                            if u.synthetic_reason
                                == Some(xai_grok_sampling_types::SyntheticReason::SystemReminder)
                    )
                    .then(|| item.text_content())
                })
                .expect("workflow-only reminder");
            assert!(reminder.contains("deep-research"), "{reminder}");
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn subagent_session_does_not_list_workflows() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.background_workflows_enabled = true;
            actor.startup_hints.is_subagent = true;
            seed_skills(&actor, &["commit"]).await;
            let mut conversation = vec![ConversationItem::system("sys")];
            actor
                .inject_baseline_skill_reminder(&mut conversation)
                .await;
            let reminder = conversation
                .iter()
                .find_map(|item| {
                    matches!(
                        item,
                        ConversationItem::User(u)
                            if u.synthetic_reason
                                == Some(xai_grok_sampling_types::SyntheticReason::SystemReminder)
                    )
                    .then(|| item.text_content())
                })
                .expect("skill reminder");
            assert!(reminder.contains("commit"), "{reminder}");
            assert!(
                !reminder.contains("deep-research"),
                "subagents cannot launch workflows:\n{reminder}"
            );
            assert!(actor.workflow_listing_for_prompt().is_none());
        })
        .await;
}
/// This test pins the MCP row against drift.
/// The estimated snapshot must equal the body `maybe_inject_mcp_reminder` injects in `Full` mode, minus the `<system-reminder>` wrapper.
/// Composing the two texts differently (for example, dropping the tool usage hint from one side) fails this test.
#[tokio::test(flavor = "current_thread")]
async fn mcp_snapshot_matches_full_mode_injected_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.mcp_reminder_mode = McpReminderMode::Full;
            install_mcp_servers(&actor);
            let snapshot = actor
                .mcp_announcement_snapshot()
                .await
                .expect("servers installed");
            assert_eq!(snapshot.server_count, 1);
            actor
                .mcp_reminder_dirty
                .store(true, std::sync::atomic::Ordering::Relaxed);
            actor.maybe_inject_mcp_reminder().await;
            let conversation = actor.chat_state_handle.get_conversation().await;
            let injected = conversation
                .last()
                .expect("reminder injected")
                .text_content();
            let body = injected
                .strip_prefix("<system-reminder>\n")
                .and_then(|s| s.strip_suffix("\n</system-reminder>"))
                .unwrap_or_else(|| panic!("unexpected wrapper: {injected}"));
            assert_eq!(body, snapshot.text);
        })
        .await;
}

/// The memory-search block is the one injection that lives inside the system
/// message rather than the first user turn, and it is recovered from there
/// rather than re-searched — so `/context` reports the block the model was
/// actually sent, at the size it actually is.
#[tokio::test(flavor = "current_thread")]
async fn the_memory_search_block_is_read_back_out_of_the_system_message() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let block = crate::session::helpers::memory_context::format_memory_reminder(&[
                xai_grok_tools::types::memory_backend::MemorySearchResult {
                    chunk_id: "test:0".into(),
                    path: "/mem/MEMORY.md".into(),
                    start_line: 0,
                    end_line: 3,
                    score: 0.85,
                    snippet: "Project uses Rust.".into(),
                    source: "workspace".into(),
                    created_at: None,
                },
            ])
            .expect("one result renders a block");
            let system = ConversationItem::system(format!("You are an agent.\n\n{block}"));

            let rows = actor.usage_categories(Some(&system)).await;
            let row = rows.first().expect("the memory row leads the list");
            assert_eq!(row.label, "Memory search");
            assert_eq!(row.detail.as_deref(), Some("1 result"));
            assert_eq!(
                row.text.as_deref(),
                Some(block.as_str()),
                "the row must carry the block itself, not the system prompt around it"
            );
            assert_eq!(
                row.tokens,
                xai_token_estimation::estimate_tokens(&block),
                "the count must be of the text the row carries"
            );

            assert!(
                actor.usage_categories(None).await.is_empty(),
                "no system message means no memory row, not an empty one"
            );
        })
        .await;
}

/// `x.ai/session/info` carries the window already resolved into the rows,
/// bands and thresholds `/context` draws, so the terminal and any other client
/// render the same numbers instead of each deriving their own.
#[tokio::test(flavor = "current_thread")]
async fn session_info_carries_the_resolved_context_facts() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let facts = actor
                .build_session_info()
                .await
                .context_facts
                .expect("session/info resolves the facts");
            assert_eq!(facts.total, 256_000);
            assert_eq!(facts.auto_compact.threshold_percent, 85);
            assert_eq!(
                facts.auto_compact.threshold_tokens,
                256_000 * 85 / 100,
                "the trigger the agent will actually fire on, in tokens"
            );
            assert_eq!(
                facts.bar.used() + facts.bar.free,
                crate::session::BarPartition::UNITS,
                "the partition a client draws must be exact"
            );
            let labels: Vec<&str> = facts
                .contributors
                .iter()
                .map(|c| c.label.as_str())
                .collect();
            assert!(labels.contains(&"System prompt"), "{labels:?}");
            assert!(labels.contains(&"Free"), "{labels:?}");
        })
        .await;
}

/// The compaction history comes off the session's own log, not out of a
/// client's memory: an agent that never saw these compactions run still
/// reports them, which is what lets a browser or a freshly attached pager
/// show the same section the terminal has always shown.
#[tokio::test(flavor = "current_thread")]
async fn session_info_reports_compactions_read_back_from_the_session_log() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // Two compactions this actor never ran, exactly as the notification
            // that announced them was persisted.
            let path = actor.transcript_path();
            std::fs::create_dir_all(path.parent().expect("session dir")).expect("mkdir");
            let line = |before: u64, after: u64| {
                let notif = crate::extensions::notification::SessionNotification {
                    session_id: actor.session_info.id.clone(),
                    update: crate::extensions::notification::SessionUpdate::AutoCompactCompleted {
                        tokens_before: Some(before),
                        tokens_after: after,
                        elapsed_ms: Some(400),
                        summary_preview: None,
                    },
                    meta: None,
                };
                serde_json::to_string(
                    &crate::session::storage::SessionUpdateEnvelope::from_update(
                        &crate::session::storage::SessionUpdate::Xai(Box::new(notif)),
                    )
                    .expect("envelope"),
                )
                .expect("json")
            };
            std::fs::write(
                &path,
                format!("{}\n{}\n", line(200_000, 20_000), line(180_000, 15_000)),
            )
            .expect("write log");
            actor.signals_handle().record_compaction(200_000);
            actor.signals_handle().record_compaction(180_000);

            let facts = actor
                .build_session_info()
                .await
                .context_facts
                .expect("session/info resolves the facts");
            assert_eq!(facts.compaction.reported_count, 2);
            assert_eq!(facts.compaction.records.len(), 2);
            assert_eq!(facts.compaction.undetailed(), 0);
            assert_eq!(facts.compaction.recovered_tokens, 180_000 + 165_000);
            assert_eq!(facts.compaction.elapsed_ms, 800);
            assert_eq!(facts.compaction.records[0].tokens_after, 20_000);
            assert_eq!(facts.compaction.records[1].ordinal, 2);
        })
        .await;
}
