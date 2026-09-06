//! Mid-turn interjection concern for `SessionActor` (buffer type, formatting,
//! broadcast, drain). Also hosts `inject_synthetic_user_message`, the shared
//! synthetic-user-message injector the permission-panel followup path reuses.

use super::*;

// Buffer, entry type, and formatting live in the shared
// xai-interjection-core crate so the server-side agent loop can adopt the
// same semantics. The shell keeps arrival (ACP ext methods), persistence,
// and pager echo.
//
// Re-exported for `acp_session.rs` which does `pub(crate) use interjection::*;`
// so retained code and co-located tests keep resolving by `acp_session::` path.
#[allow(unused_imports)]
pub(crate) use xai_interjection_core::{
    INTERRUPT_NOTE, InterjectionBuffer, drain_formatted, format_interjection, frame_user_turn,
};

/// Shell instantiation of the shared entry type: images are ACP content.
pub(crate) type PendingInterjection = xai_interjection_core::PendingInterjection<acp::ImageContent>;

/// A buffered steering message plus the channel that reports its fate.
///
/// Deliberately not a [`PendingInterjection`]: an interjection is the user's
/// own text and is framed as a `<user_query>` the model may weigh against its
/// in-flight work, while this is the session owner speaking and is framed as a
/// `<system-reminder>`. Keeping them in separate buffers also keeps the
/// interjection path — which converts strays into prompt turns — from ever
/// resurrecting a steering message as a turn of its own.
pub(crate) struct PendingSteeringMessage {
    pub(crate) text: String,
    /// Who is speaking, which decides the framing at drain time.
    pub(crate) origin: SteeringOrigin,
    /// `true` once the text is in the conversation; `false` if the turn ended
    /// or was cancelled first. Dropped without a send only when the session
    /// actor itself goes away, which the sender reads as unreachable.
    ///
    /// `None` for an entry whose sender was already answered at enqueue — a
    /// child's report, which is acknowledged as *taken* rather than as read so
    /// the child does not park behind a parent that may be awaiting it.
    pub(crate) ack: Option<tokio::sync::oneshot::Sender<bool>>,
}

/// Which direction a buffered message came from.
///
/// Both ride the same buffer and the same drain points; only the framing and
/// the standing they claim differ. Keeping the distinction in the entry rather
/// than in pre-formatted text means the truncation and the wrapper stay in one
/// place.
pub(crate) enum SteeringOrigin {
    /// The agent that owns this session, correcting it.
    Owner,
    /// A subagent this session spawned, reporting up mid-task.
    Child { subagent_id: String },
}

/// Wrap steering text for the model. The framing names the sender's standing —
/// the agent that owns this session, not the requester of its task — so the
/// model treats it as a correction to follow rather than a change of mind to
/// weigh. Truncated on the same threshold as an interjection so one oversized
/// message cannot displace the turn's own context.
///
/// A child's report is framed the opposite way. It comes from below, so it is
/// information rather than instruction, and the text says so — including that
/// the child is still working and is not waiting for an answer. A parent that
/// reads a report as a question to reply to is the first half of a ping-pong;
/// the cap on a child's outbound messages bounds that regardless, but the
/// framing is what keeps the ordinary case from starting one.
pub(crate) fn format_steering_message(text: String, origin: &SteeringOrigin) -> String {
    let truncated = xai_interjection_core::truncate_large_prompt(text);
    match origin {
        SteeringOrigin::Owner => format!(
            "The agent that started this task sent a correction while you were \
             working. Treat it as an instruction, not as new information:\n{truncated}"
        ),
        SteeringOrigin::Child { subagent_id } => format!(
            "Subagent {subagent_id}, which you spawned and which is still running, \
             sent this while working. It is a report, not an instruction, and it is \
             not waiting for a reply — it has already gone back to work. Take it \
             into account; answer only if it changes what that subagent or another \
             one should be doing, and then by messaging that subagent:\n{truncated}"
        ),
    }
}

/// Prompt-id prefix for interjections that missed their turn and were
/// converted into standalone prompt turns (arrived while idle, or after the
/// running turn's final drain). The prefix keeps the turn's user echo
/// persist-only: every pane already rendered the text from the
/// `x.ai/session/interjection` broadcast, so a live echo would duplicate it.
pub(crate) const INTERJECT_FALLBACK_PROMPT_PREFIX: &str = "interject-fallback-";

pub(crate) fn is_interject_fallback(prompt_id: &str) -> bool {
    prompt_id.starts_with(INTERJECT_FALLBACK_PROMPT_PREFIX)
}

impl SessionActor {
    /// Convert a stranded interjection into a queued prompt turn.
    ///
    /// An interjection is only merged into a *running* turn
    /// (`drain_pending_interjections`); one that arrives while the session is
    /// idle — or lands after the running turn's final drain — would otherwise
    /// sit in `pending_interjections` forever and the user's message would be
    /// silently lost (the pager already rendered it and said "Interjection
    /// sent"). Queue it as its own prompt turn instead; the caller kicks
    /// `maybe_start_running_task`.
    ///
    /// `front` puts the converted turn ahead of already-queued prompts —
    /// send-now semantics: the user asked for "now", queued rows asked for
    /// "later". Front placement is re-validated under the state lock: the
    /// caller's "no turn running" check is unlocked, so a concurrent
    /// promotion (MCP-init release, plan-approval resume) may have pinned a
    /// running prompt at the front in the meantime — displacing it would
    /// desync `handle_completion`'s front pop. In that case the item lands
    /// right behind the running front.
    pub(super) async fn queue_interjection_fallback_prompt(
        &self,
        text: String,
        images: Vec<acp::ImageContent>,
        front: bool,
    ) {
        let prompt_id = format!("{INTERJECT_FALLBACK_PROMPT_PREFIX}{}", uuid::Uuid::now_v7());
        let mut prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(text))];
        prompt_blocks.extend(images.into_iter().map(acp::ContentBlock::Image));
        // Respect an active plan mode: the interjection was aimed at a turn
        // that ran under it, so its fallback turn must not escape the gate.
        let prompt_mode = if self.plan_mode.lock().is_active() {
            crate::session::plan_mode::PromptMode::Plan
        } else {
            crate::session::plan_mode::PromptMode::Agent
        };
        let (respond_to, _) = tokio::sync::oneshot::channel();
        // User message (skips queue_input); invalidate in-flight recap now.
        self.invalidate_side_calls_for_new_prompt();
        let item = InputItem {
            prompt_id,
            prompt_blocks,
            prompt_mode,
            trace_gcs_config: None,
            artifact_tracker: None,
            client_identifier: None,
            screen_mode: None,
            verbatim: false,
            json_schema: None,
            input_origin: InputOrigin::new(super::super::PromptOrigin::User),
            task_wake_fallback: None,
            tool_overrides_update: None,
            respond_to,
            persist_ack: None,
            parsed_prompt_tx: None,
            queue_meta: None,
            queue_mutation_policy: QueueMutationPolicy::hidden(),
            // Send-now semantics (see doc): a later real send-now must not
            // leapfrog this fallback in `queue_input`'s FIFO scan.
            send_now: front,
        };
        let mut state = self.state.lock().await;
        if front {
            // Never displace a running front (see doc): insert after it when
            // the front row is the in-flight turn's own item.
            let insert_at = usize::from(matches!(
                (state.pending_inputs.front(), state.running_prompt_id()),
                (Some(front_item), Some(running)) if front_item.prompt_id == running
            ));
            state.pending_inputs.insert(insert_at, item);
        } else {
            state.pending_inputs.push_back(item);
        }
        tracing::info!("Converted stranded interjection into a queued prompt turn");
    }

    /// Take an out-of-band steering message aimed at the running turn.
    ///
    /// The same running-turn test [`SessionCommand::Interject`] makes, with the
    /// opposite fallback: an interjection that finds no turn becomes a prompt
    /// turn of its own, while a steering message is answered `false` and
    /// dropped. Nothing is acknowledged here on the accepting path — the drain
    /// does that once the text is actually in the conversation.
    pub(crate) fn accept_steering_message(
        &self,
        text: String,
        ack: tokio::sync::oneshot::Sender<bool>,
    ) {
        let turn_running = self
            .current_prompt_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .is_some();
        if !turn_running {
            let _ = ack.send(false);
            tracing::info!("Dropped steering message: no turn running in this session");
            return;
        }
        self.pending_steering.lock().push(PendingSteeringMessage {
            text,
            origin: SteeringOrigin::Owner,
            ack: Some(ack),
        });
        tracing::info!("Queued out-of-band steering message");
    }

    /// Take a running subagent's report into this session's turn.
    ///
    /// The same running-turn test as [`Self::accept_steering_message`], and the
    /// same drop when there is none: a session between turns is not woken by a
    /// child, only by that child's completion, which is gated on whether
    /// anything is awaiting it.
    ///
    /// The ack fires here rather than at drain — the one place this path
    /// deliberately differs. A parent blocked inside the `task` call that
    /// awaits this very child reaches no drain point until the child finishes,
    /// so a delivery ack would be the child waiting on itself.
    pub(crate) fn accept_child_report(
        &self,
        subagent_id: String,
        text: String,
        ack: tokio::sync::oneshot::Sender<bool>,
    ) {
        let turn_running = self
            .current_prompt_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .is_some();
        if !turn_running {
            let _ = ack.send(false);
            tracing::info!(
                subagent_id,
                "Dropped subagent report: no turn running in the parent session"
            );
            return;
        }
        self.pending_steering.lock().push(PendingSteeringMessage {
            text,
            origin: SteeringOrigin::Child { subagent_id },
            ack: None,
        });
        let _ = ack.send(true);
        tracing::info!("Queued a running subagent's report into the parent's turn");
    }

    /// Drain buffered steering messages into the conversation as
    /// `<system-reminder>` items and acknowledge each one. Returns `true` if
    /// anything was drained, so the caller can `continue` the turn loop and let
    /// the model act on the correction.
    ///
    /// Called at exactly the drain points [`Self::drain_pending_interjections`]
    /// uses; the ack fires here rather than at enqueue so "delivered" means the
    /// model will see it, not that a channel accepted it.
    pub(super) fn drain_pending_steering(&self) -> bool {
        let entries = std::mem::take(&mut *self.pending_steering.lock());
        if entries.is_empty() {
            return false;
        }
        for PendingSteeringMessage { text, origin, ack } in entries {
            self.push_system_reminder(&format_steering_message(text, &origin));
            if let Some(ack) = ack {
                let _ = ack.send(true);
            }
        }
        tracing::info!("Injected out-of-band steering message(s) into the running turn");
        true
    }

    /// Answer every buffered steering message with "not delivered" and drop it.
    ///
    /// Used on the cancel paths, where the turn the messages were aimed at was
    /// taken away rather than allowed to finish. Dropping is the point: a
    /// cancel means the model stops, so nothing here may survive into whatever
    /// runs next, and the owner is told so rather than left believing it
    /// steered. A child's report goes with it, which is the one thing
    /// `message_parent` warns the child about in as many words — it promises
    /// delivery "unless that turn is cancelled first" — so the send it spent
    /// buys the outcome the child was told it might.
    ///
    /// A turn that simply *ends* is not this case; see
    /// [`Self::discard_steering_at_turn_end`].
    pub(super) fn discard_pending_steering(&self) {
        let entries = std::mem::take(&mut *self.pending_steering.lock());
        if entries.is_empty() {
            return;
        }
        let count = entries.len();
        for entry in entries {
            // A child's report was already answered "taken" at enqueue and
            // carries no channel here; only the owner's steering is waiting to
            // hear that its turn is gone.
            if let Some(ack) = entry.ack {
                let _ = ack.send(false);
            }
        }
        tracing::info!(count, "Discarded steering message(s) with no turn to steer");
    }

    /// Turn-end counterpart of [`Self::discard_pending_steering`]: answer the
    /// owner's steering "not delivered", and carry a child's report over to
    /// whatever turn the session runs next.
    ///
    /// The two entries were promised different things, so a turn ending
    /// normally has to treat them differently. The owner's steering was aimed
    /// at *that* turn, its sender is still waiting on an answer, and there is
    /// nothing honest to do but answer `false`. A child's report was already
    /// answered "taken" at enqueue and one of the child's three sends was spent
    /// on it; the only escape clause it was given was a cancel. Dropping it
    /// here — the reachable case being a report that arrives while the parent
    /// is in turn-end bookkeeping, past its final drain — spends the send on
    /// nothing and reports a delivery that never happened.
    ///
    /// Holding it costs the child nothing it has not already paid and breaks no
    /// rule the reverse channel rests on. Nothing here starts a turn, so an
    /// idle parent is still never woken by a child; the text simply waits for
    /// the parent's next step, which is what the child was told it would get.
    /// The alternative — crediting the send back — would need the parent's
    /// session actor to reach into the coordinator's registry to undo an
    /// accounting decision the child cannot see, to compensate for a message
    /// that is still perfectly deliverable.
    pub(super) fn discard_steering_at_turn_end(&self) {
        let mut pending = self.pending_steering.lock();
        let mut carried = 0usize;
        let mut answered = 0usize;
        pending.retain_mut(|entry| match entry.origin {
            SteeringOrigin::Child { .. } => {
                carried += 1;
                true
            }
            SteeringOrigin::Owner => {
                if let Some(ack) = entry.ack.take() {
                    let _ = ack.send(false);
                }
                answered += 1;
                false
            }
        });
        if answered > 0 || carried > 0 {
            tracing::info!(
                answered,
                carried,
                "Turn ended under buffered steering message(s)"
            );
        }
    }

    /// Convert interjections that missed their turn's final drain into queued
    /// prompt turns, front of the queue in original order. Returns the count.
    pub(super) async fn flush_stranded_interjections(&self) -> usize {
        let stranded = self.pending_interjections.drain_all();
        let count = stranded.len();
        // Reversed push_fronts keep entry 0 front-most.
        for entry in stranded.into_iter().rev() {
            self.queue_interjection_fallback_prompt(entry.text, entry.attachments, true)
                .await;
        }
        count
    }
    /// Normalize interjection images for injection (shared pipeline above);
    /// notices append to `wrapped` (TEXT side only). Returns the images to
    /// attach structurally. Sessions whose template rejects inline images
    /// instead transcribe normalized survivors into the text via the existing
    /// describe pipeline, or drop them with a notice.
    async fn prepare_interjection_images(
        &self,
        wrapped: &mut String,
        images: Vec<acp::ImageContent>,
    ) -> Vec<acp::ImageContent> {
        if images.is_empty() {
            return images;
        }
        let is_cursor = self.is_cursor_harness();
        let images = self
            .normalize_images_with_notices(wrapped, images, is_cursor)
            .await;
        if !is_cursor {
            return images;
        }
        if !images.is_empty() {
            match self.transcribe_user_images(wrapped.clone(), &images).await {
                Ok(new_text) => *wrapped = new_text,
                Err(e) => {
                    tracing::warn!(?e, "interjection image processing failed; dropping images");
                    wrapped.push_str(
                        "\n\n[Note: the user attached image(s) to this message, but they could \
                         not be processed in this session and were dropped.]",
                    );
                }
            }
        }
        Vec::new()
    }

    /// Broadcast a mid-turn interjection to every attached client.
    /// The originator uses `id` to claim its optimistic prompt block; other
    /// clients render the notification normally.
    pub(super) fn broadcast_interjection(&self, text: &str, id: Option<&str>) {
        let mut payload = serde_json::json!({
            "sessionId": self.session_info.id.0.as_ref(),
            "text": text,
        });
        if let Some(id) = id {
            payload["interjectionId"] = serde_json::json!(id);
        }
        if let Ok(params) = serde_json::value::to_raw_value(&payload) {
            self.notifications
                .gateway
                .forward_fire_and_forget(acp::ExtNotification::new(
                    "x.ai/session/interjection",
                    params.into(),
                ));
        }
    }

    /// Inject a synthetic user message: persist, optionally notify pager, push
    /// notifies) and `drain_pending_interjections` (which skips notification
    /// `<skill_information>` envelope (loaded + substituted SKILL.md bodies).
    /// because the pager already has a local user prompt block).
    pub(super) async fn inject_synthetic_user_message(
        &self,
        text: &str,
        item: ConversationItem,
        notify_pager: bool,
        images: &[acp::ImageContent],
    ) {
        let model_id = self.current_model_id().await;
        let user_chunk_meta = serde_json::json!({ "modelId": model_id })
            .as_object()
            .cloned();

        // Persist to updates.jsonl: one UserMessageChunk per content block
        // (text first, then any images — Image chunks already round-trip).
        let mut content_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
            text.to_string(),
        ))];
        content_blocks.extend(images.iter().cloned().map(acp::ContentBlock::Image));
        let notification_meta = self.build_notification_meta();
        for content_block in content_blocks {
            let update = acp::SessionUpdate::UserMessageChunk(
                acp::ContentChunk::new(content_block).meta(user_chunk_meta.clone()),
            );
            let _ = self
                .notifications
                .persistence_tx
                .send(PersistenceMsg::Update(SessionUpdate::Acp(Box::new(
                    acp::SessionNotification::new(self.session_info.id.clone(), update)
                        .meta(notification_meta.clone().as_object().cloned()),
                ))));
        }

        // Notify pager (skipped for interjections — pager has local block).
        if notify_pager {
            self.send_update(
                acp::SessionUpdate::UserMessageChunk(
                    acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                        text.to_string(),
                    )))
                    .meta(user_chunk_meta),
                ),
                None,
            )
            .await;
        }

        // Add to conversation context
        self.chat_state_handle.push_user_message(item);
    }

    /// Expand skill slash references in interjection text into the
    ///
    /// Interjections bypass turn-start slash resolution
    /// (`slash_commands::resolve`), so without this a queued `/skill` row
    /// force-sent mid-turn — or a typed `/skill` interjection — reaches the
    /// model as a bare, unexpanded slash command. Returns `None` when the
    /// conversation as a standalone synthetic user message
    /// text references no known skill.
    async fn interjection_skill_information(&self, text: &str) -> Option<String> {
        // Mirror turn-start gating (`parse_slash_prefix`): only a leading
        // slash invokes skills — "don't run /commit yet" is steering text,
        // not an invocation.
        if !text.trim_start().starts_with('/') {
            return None;
        }
        let slash_skills = self.slash_skills_for_resolve().await;
        // Availability without `command_availability()`'s goal-reconciliation
        // side effects — this runs mid-turn inside the drain.
        let tool_names = self.registered_tool_names().await;
        let has_workflow_runs = !self.workflow_tracker().await.lock().list().is_empty();
        let availability = self.build_command_availability(&tool_names, has_workflow_runs);
        let parsed = slash_commands::parse_skill_references(text, &slash_skills, availability)?;
        // Deliberately lighter telemetry than turn start: no `skill.activated`
        // span, `PluginUsed`, or `active_skill` stamp — those attribute the
        // turn, which this skill did not start. `SkillDispatched` still
        // carries `plugin_source`, so dispatch counts stay complete.
        for sk in &parsed {
            xai_grok_telemetry::session_ctx::log_event(
                xai_grok_telemetry::events::SlashCommandUsed {
                    command: sk.name.clone(),
                    args_provided: !sk.args.is_empty(),
                },
            );
            xai_grok_telemetry::session_ctx::log_event(
                xai_grok_telemetry::events::SkillDispatched {
                    skill_name: sk.name.clone(),
                    plugin_source: sk.plugin_name.clone(),
                    trigger: xai_grok_telemetry::events::SkillTrigger::SlashCommand,
                },
            );
        }
        slash_commands::build_skill_information_for_refs(
            &parsed,
            &slash_skills,
            &self.session_id_string(),
        )
        .await
    }

    /// When follow-up behavior is Steer, promote held queue rows into
    /// interjections, then drain. Call after a tool batch, at loop top, and
    /// before the turn returns to the user. Returns `true` if any
    /// interjections were drained (caller may `continue` so the model sees them
    /// next).
    pub(super) async fn drain_interjections_at_safe_point(&self) -> bool {
        // Queue (default) must not re-parse config on every tool/model/turn-end
        // drain. `follow_up_steer_enabled` is mtime-keyed on config.toml so a
        // live pager settings write is visible without restarting the shell
        // agent (a separate process); unchanged mtime is a cheap stat.
        if crate::util::config::follow_up_steer_enabled().await {
            let has_held = {
                let state = self.state.lock().await;
                let running = state.running_prompt_id();
                // Only editable human rows are promotable; protected pins and
                // queue-hidden fallbacks must not arm steer promotion.
                running.is_some()
                    && state.pending_inputs.iter().any(|item| {
                        item.is_queue_editable() && Some(item.prompt_id.as_str()) != running
                    })
            };
            if has_held {
                self.promote_queued_as_interjections().await;
            }
        }
        self.drain_pending_interjections().await
    }

    pub(super) async fn drain_pending_interjections(&self) -> bool {
        // Manual drain (not `drain_formatted`): skill parsing needs the raw
        // text — parsed post-wrap, the envelope's closing `</user_query>` tag
        // would pollute the trailing skill's args.
        let entries = self.pending_interjections.drain_all();
        if entries.is_empty() {
            return false;
        }

        for PendingInterjection { text, attachments } in entries {
            // Sanitizer drops `[Image #N: <path>]` → `[Image #N]` before the
            // text reaches the model, covering legacy-client raw text AND the
            // queue-interject harvest. Wrapping and truncation stay in the
            // shared crate (`format_interjection`).
            let sanitized =
                crate::session::placeholder_images::strip_paths_from_image_placeholders(text);
            let skill_information = self.interjection_skill_information(&sanitized).await;
            let mut wrapped = format_interjection(sanitized);
            let images = self
                .prepare_interjection_images(&mut wrapped, attachments)
                .await;
            // Model-visible text: <skill_information> follows the wrapped
            // <user_query> — same order as turn-start prompt assembly, and
            // appended after the image pipeline so the template-specific
            // transcription rewrite cannot mangle the envelope. The
            // persisted user chunk stays envelope-free so session replay
            // renders the compact interjection, not the SKILL.md body
            // (mirrors turn-start skills, which replay via `displayText`).
            let model_text = match &skill_information {
                Some(skill_information) => {
                    tracing::info!("expanded skill references in mid-turn interjection");
                    format!("{wrapped}\n{skill_information}")
                }
                None => wrapped.clone(),
            };
            let mut item = ConversationItem::interjection(model_text);
            for img in &images {
                item.add_image(pick_user_image_url(img));
            }
            self.inject_synthetic_user_message(&wrapped, item, false, &images)
                .await;
            tracing::info!("Injected mid-turn interjection as standalone synthetic user message");
        }
        // An interjection never cancels the turn, so it leaves no marker on the
        // next user turn (that field is reserved for fatal aborts). The
        // interjection itself is recorded at enqueue time via
        // `Event::Interjected` (carrying the shared `redirect_kind`).
        true
    }
}
