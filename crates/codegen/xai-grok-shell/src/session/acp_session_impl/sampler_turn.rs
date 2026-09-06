//! Sampler-turn pipeline for `SessionActor`: tool definitions, model auth facts/gates and retry, and sampler config reconstruction.
//! It also covers sampling-failure recovery and per-response usage recording.

use super::*;
use crate::auth::backend::{ActiveAuthBackend, AuthBackend};
use xai_grok_telemetry::region;
use xai_grok_telemetry::region::Parent;

const CLASSIFIER_REQUEST_TOKEN_RESERVE: u64 = 16_384;

fn classifier_request_fits_context(input_tokens: u64, context_window: u64) -> bool {
    input_tokens <= context_window.saturating_sub(CLASSIFIER_REQUEST_TOKEN_RESERVE)
}

/// Per-prompt cap on the Length tool-call salvage streak. Matches the agent implementation's `MAX_RETRY_ITERATIONS`.
pub(super) const MAX_OUTPUT_TOKEN_LIMIT_RETRIES: u32 = 5;

/// Resubmits per sampling step: automates the manual "Continue" that revives dead sessions.
pub(super) const MAX_TRANSIENT_TURN_RETRIES: u32 = 3;

/// Cumulative resubmit cap for the whole prompt.
/// Long agentic prompts re-earn the per-step budget every successful sample, so without this a partial outage multiplies retries by round count.
/// It is prompt-scoped (an actor `Cell`, not a turn-loop local).
/// Auto-recovery, stop-hook continuations, and the goal loop re-enter `process_conversation_turn` within one prompt and would reset a local.
pub(super) const MAX_TRANSIENT_RETRIES_PER_PROMPT: u32 = 10;

/// Wall-clock budget per recovery episode (first transient failure after a success until the next success).
/// This bounds how many idle stalls can stack up: each stalled attempt burns a full idle-detector cycle before it even fails.
pub(super) const MAX_TRANSIENT_RETRY_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);

/// Turn-loop retry state for the transient arm.
/// The window is evaluated at failure time (`tokio::time::Instant` so paused-clock tests can drive it).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TransientRetryState {
    /// Resubmits used this sampling step (resets on success/compaction).
    pub(crate) step_attempts: u32,
    /// Resubmits used across the whole prompt (never resets mid-prompt).
    pub(crate) prompt_attempts: u32,
    /// First transient failure of the current recovery episode (`None` until one happens; cleared on success).
    pub(crate) episode_start: Option<tokio::time::Instant>,
    /// Spawn-resolved kill switch (foreground root sessions only).
    pub(crate) enabled: bool,
}

/// Ceiling shown to the user (`Retrying (N/M)`): attempts used this step plus whatever further attempts both remaining budgets allow.
pub(super) fn transient_display_ceiling(step_attempts: u32, prompt_attempts: u32) -> u32 {
    step_attempts
        + MAX_TRANSIENT_TURN_RETRIES
            .saturating_sub(step_attempts)
            .min(MAX_TRANSIENT_RETRIES_PER_PROMPT.saturating_sub(prompt_attempts))
}

impl TransientRetryState {
    fn budget_remaining(&self) -> bool {
        self.step_attempts < MAX_TRANSIENT_TURN_RETRIES
            && self.prompt_attempts < MAX_TRANSIENT_RETRIES_PER_PROMPT
            && self
                .episode_start
                .is_none_or(|s| s.elapsed() < MAX_TRANSIENT_RETRY_WINDOW)
    }
}

/// Slower than the sampler's ladder (already spent).
/// Jittered at the sleep site: idle timeouts skip the sampler's jitter and fire in lockstep.
pub(super) const TRANSIENT_TURN_RETRY_BACKOFF: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(10),
    std::time::Duration::from_secs(30),
];

const _: () = assert!(MAX_TRANSIENT_TURN_RETRIES as usize == TRANSIENT_TURN_RETRY_BACKOFF.len());

/// Delay before resubmit `attempts_used + 1`, clamped to the last rung.
pub(super) fn transient_backoff_delay(attempts_used: u32) -> std::time::Duration {
    TRANSIENT_TURN_RETRY_BACKOFF[usize::min(
        attempts_used as usize,
        TRANSIENT_TURN_RETRY_BACKOFF.len() - 1,
    )]
}

/// Stream stalls (never retried internally), transport errors, retryable 5xx.
/// Vetoes mirror `is_retry_vetoed`. Status-less `Api` fails closed; other kinds keep their dedicated recovery or terminal path.
pub(super) fn transient_retry_eligible(error: &xai_grok_sampler::SamplingErrorInfo) -> bool {
    use xai_grok_sampler::SamplingErrorKind;
    if error.should_retry == Some(false)
        || xai_grok_sampling_types::is_context_length_error(&error.message)
        // Deterministic rejection however the proxy wrapped it (400 or 500); the sampler's own classifier already stripped images and gave up
        || matches!(
            error.error_code,
            Some(xai_grok_sampling_types::ApiErrorCode::InvalidImage)
        )
    {
        return false;
    }
    match error.kind {
        // `clone_error` erases reqwest retryability, so all Http kinds retry here (bounded)
        SamplingErrorKind::IdleTimeout | SamplingErrorKind::Http => true,
        // The canonical retryable-status rule; 429 is excluded explicitly because it classifies as `RateLimited`
        SamplingErrorKind::Api => error.status_code.is_some_and(|status| {
            status != 429
                && reqwest::StatusCode::from_u16(status)
                    .is_ok_and(xai_grok_sampling_types::is_retryable_api_status)
        }),
        SamplingErrorKind::Auth
        | SamplingErrorKind::Serialization
        | SamplingErrorKind::RateLimited
        | SamplingErrorKind::EmptyResponse
        | SamplingErrorKind::MaxTokensTruncation
        | SamplingErrorKind::DoomLoopDetected => false,
    }
}

/// Injected once per Length tool-call salvage streak so the model knows its output was cut mid-turn.
/// The text-continuation path injects `length_salvage::LENGTH_CONTINUE_REMINDER_BODY` instead.
pub(super) const OUTPUT_TOKEN_LIMIT_REMINDER: &str = "Your response was cut off because it exceeded the output token limit. \
     Please break your work into smaller pieces. Continue from where you left off.";

/// Consecutive Length-salvaged tool-call samples within one prompt.
/// Each salvage grows the context, so under a hard window cap an unbounded streak could loop forever once compaction stops freeing room.
/// Past the cap the turn fails like it did before salvage existed.
#[derive(Default)]
pub(super) struct LengthSalvageStreak {
    consecutive: u32,
}

/// What the turn loop does with a completed sample, per the salvage streak.
#[derive(Debug)]
pub(super) enum LengthSalvageAction {
    /// Not a Length-salvaged tool-call sample; the streak resets.
    NotSalvage,
    /// Execute the salvaged calls.
    /// `inject_reminder` is set on the first salvage of a streak (the reminder stays in context afterwards).
    Proceed { inject_reminder: bool },
    /// The streak exceeded [`MAX_OUTPUT_TOKEN_LIMIT_RETRIES`]; fail the turn.
    Exhausted,
}

impl LengthSalvageStreak {
    pub(super) fn on_sample(&mut self, length_with_tool_calls: bool) -> LengthSalvageAction {
        if !length_with_tool_calls {
            self.consecutive = 0;
            return LengthSalvageAction::NotSalvage;
        }
        self.consecutive += 1;
        if self.consecutive > MAX_OUTPUT_TOKEN_LIMIT_RETRIES {
            LengthSalvageAction::Exhausted
        } else {
            LengthSalvageAction::Proceed {
                inject_reminder: self.consecutive == 1,
            }
        }
    }
}

/// Auth-failure detector for tool errors.
/// Matches strictly on HTTP 401 when the error carries a structured status code, mirroring `SamplingError::is_auth_error` in xai-grok-sampling-types.
/// 403 is deliberately excluded because it means "authenticated but forbidden" (content-safety blocks, ZDR-gated requests, remote settings gates).
/// A token refresh there would be a no-op and would show the client a spurious auth_required teardown.
///
/// String fallbacks remain for tools that report auth failures without going through the structured `HttpFailure` path.
/// Examples: JSON-only `invalid_token` payloads, BYOK key-validation messages.
pub(super) fn is_auth_tool_error(err: &xai_tool_runtime::ToolError) -> bool {
    // When the error carries a structured HTTP status code in details, trust it as the authoritative signal
    // This replaces the legacy `HttpFailure { status, .. }` variant matching.
    if let Some(details) = &err.details
        && let Some(status) = details
            .get(HTTP_STATUS_DETAILS_KEY)
            .and_then(|s| s.as_u64())
    {
        return status == 401;
    }
    // String fallback for errors without structured status (e.g. BYOK key-validation messages, OAuth `invalid_token` payloads).
    let lower = err.to_string().to_ascii_lowercase();
    lower.contains("unauthorized")
        || lower.contains("invalid api key")
        || lower.contains("invalid_token")
}

/// Told to the model when the Auto-mode classifier's verdict schema rides as a
/// tool rather than as a native response schema.
const CLASSIFIER_STRUCTURED_OUTPUT_INSTRUCTION: &str = "Return your verdict by calling the \
     `StructuredOutput` tool exactly once, with the verdict JSON as its arguments. Do not \
     answer with text.";

/// Build the one-shot Auto-mode permission classifier request for `backend`.
///
/// The classifier exists to obtain an *enforceable* `{thinking, shouldBlock,
/// reason}` verdict, so the schema has to reach the wire in whichever shape the
/// backend actually honours. Two shapes, picked by
/// [`ApiBackend::enforces_schema_without_tools`]:
///
/// * natively, as `ConversationRequest::json_schema` — Chat Completions
///   (`response_format`), Responses (`text.format`), Gemini
///   (`generationConfig.responseJsonSchema`);
/// * otherwise as the `StructuredOutput` tool's `input_schema`, the same
///   mechanism the agent turn uses for exactly this reason (see
///   [`super::turn::STRUCTURED_OUTPUT_TOOL`]).
///
/// Messages takes the second path. Its native `output_config.format` is gated
/// behind an `anthropic-beta` opt-in this client never sends, so a schema put
/// there constrains nothing and the verdict quietly degrades to free text that
/// the caller has to guess at — the failure this split exists to prevent.
///
/// The alternative for Messages — sending the beta header — was rejected: it
/// pins a dated, provider-specific opt-in string that only Anthropic's own
/// endpoint honours, while a `[[provider]] format = "messages"` gateway would
/// still drop `output_config`. The tool schema is enforced by every
/// Messages-shaped endpoint because tool calling is not a beta anywhere.
///
/// `tool_choice` is deliberately left unset (`auto`) rather than forcing the
/// tool: the classifier enables extended thinking whenever a reasoning effort
/// resolves, and the Messages API rejects a forced `tool_choice` combined with
/// thinking. The instruction above plus the tool schema carry it instead, and
/// [`classifier_verdict_text`] still reads a text answer if the model gives one.
pub(super) fn build_permission_classifier_request(
    backend: &xai_grok_sampling_types::ApiBackend,
    model: String,
    messages: Vec<xai_grok_workspace::permission::ClassifierMessage>,
    reasoning_effort: Option<xai_grok_sampling_types::ReasoningEffort>,
    session_id: String,
) -> ConversationRequest {
    use xai_grok_workspace::permission::ClassifierMessageRole;
    let mut items = messages
        .into_iter()
        .map(|m| match m.role {
            ClassifierMessageRole::System => ConversationItem::system(m.text),
            ClassifierMessageRole::User => ConversationItem::user(m.text),
        })
        .collect::<Vec<_>>();
    let schema = xai_grok_workspace::permission::classifier_output_json_schema();
    let native = backend.enforces_schema_without_tools();
    let mut tools = Vec::new();
    if !native {
        items.push(ConversationItem::system(
            CLASSIFIER_STRUCTURED_OUTPUT_INSTRUCTION.to_owned(),
        ));
        tools.push(xai_grok_sampling_types::ToolSpec {
            name: super::turn::STRUCTURED_OUTPUT_TOOL.to_owned(),
            description: Some(super::turn::STRUCTURED_OUTPUT_TOOL_DESCRIPTION.to_owned()),
            parameters: schema.clone(),
        });
    }
    ConversationRequest {
        items,
        tools,
        hosted_tools: vec![],
        tool_choice: None,
        model: Some(model),
        temperature: None,
        max_output_tokens: None,
        json_schema: native.then_some(schema),
        reasoning_effort,
        x_grok_conv_id: Some(format!("perm-classifier-{}", uuid::Uuid::new_v4())),
        x_grok_req_id: Some(format!("xai-perm-auto-{}", uuid::Uuid::new_v4())),
        x_grok_session_id: Some(session_id),
        x_grok_agent_id: Some(xai_grok_telemetry::id::agent_id()),
        ..ConversationRequest::default()
    }
}

/// The verdict text a collected classifier response carries.
///
/// On the tool path the verdict is the `StructuredOutput` call's arguments, not
/// assistant text; on the native path it is the assistant text. Both are JSON
/// for [`xai_grok_workspace::permission::parse_classifier_model_output`], so
/// this prefers the tool call and falls back to the text a model returns when it
/// answers in prose despite the instruction.
pub(super) fn classifier_verdict_text(
    response: &xai_grok_sampling_types::ConversationResponse,
) -> String {
    let tool_args = response.items.iter().find_map(|item| match item {
        ConversationItem::Assistant(a) => a
            .tool_calls
            .iter()
            .find(|tc| tc.name == super::turn::STRUCTURED_OUTPUT_TOOL)
            .map(|tc| tc.arguments.as_ref().to_owned()),
        _ => None,
    });
    tool_args.unwrap_or_else(|| response.assistant_text())
}

/// A [`xai_grok_sampler::BearerResolver`] that always returns the same
/// pre-resolved token. Carries a plugin-supplied credential (resolved once per
/// turn in [`SessionActor::reconstruct_full_config`]) onto the sampler's
/// per-request auth-attachment path.
///
/// It reports the endpoint it was minted for, which is what lets the aux,
/// failover and subagent config paths tell this credential apart from the
/// session's own: those paths may attach it to a config for that same endpoint,
/// and to nothing else.
struct StaticBearerResolver {
    token: String,
    /// The `base_url` the plugin was asked to mint this credential for.
    endpoint: String,
}
impl std::fmt::Debug for StaticBearerResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticBearerResolver")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}
impl xai_grok_sampler::BearerResolver for StaticBearerResolver {
    fn current_bearer(&self) -> Option<String> {
        Some(self.token.clone())
    }
    fn minted_for(&self) -> Option<xai_grok_sampler::MintedFor<'_>> {
        Some(xai_grok_sampler::MintedFor {
            base_url: &self.endpoint,
            // The seam is bearer-shaped, so this is the scheme the credential
            // is usable in — see `resolve_custom_provider_auth`.
            auth_scheme: xai_grok_sampler::AuthScheme::Bearer,
        })
    }
}

/// Route a plugin-supplied credential onto a **custom (non-xAI) provider**.
///
/// When the session has a credential seam (a sidecar plugin subscribed to
/// `resolve_credential`) and the outbound `base_url` is not an xAI endpoint,
/// ask the plugin for the outbound bearer. On a hit the credential is carried
/// as `Authorization: Bearer <token>`: the seam always supplies a bearer, so
/// the provider's static `auth_scheme` (e.g. `x-api-key` for the Anthropic
/// Messages format) is overridden to `Bearer` while the request-body shape
/// (`api_backend`) is left untouched. This is what lets a plugin OAuth token
/// reach a BYOK endpoint such as `api.anthropic.com`.
///
/// Returns `(bearer_resolver, auth_scheme_override)`:
/// - `(Some(resolver), Some(Bearer))` when a plugin credential was resolved,
/// - `(None, None)` for an xAI endpoint, no seam, or a plugin passthrough — the
///   caller then keeps the provider's static `api_key`/`auth_scheme`.
///
/// xAI endpoints are never routed here (their auth stays with the session
/// `AuthManager` token), so this never perturbs the first-party path. A
/// per-provider `x-api-key` supplied by a plugin is out of scope: the seam is
/// bearer-shaped, and static keys already cover that case.
async fn resolve_custom_provider_auth(
    seam: Option<Arc<dyn crate::auth::credential_seam::PluginCredentialSeam>>,
    base_url: &str,
    auth_account: Option<&str>,
) -> (
    Option<xai_grok_sampler::SharedBearerResolver>,
    Option<xai_grok_sampler::AuthScheme>,
) {
    // First-party (xAI) endpoints keep their AuthManager-driven auth untouched.
    if crate::util::is_xai_api_url(base_url) {
        return (None, None);
    }
    let Some(seam) = seam else {
        return (None, None);
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    // `base_url` also rides the payload as a first-class field so the plugin can
    // scope its reply to this exact custom-provider endpoint, and the model's
    // `auth_account` rides it as `ownerHint` so a plugin holding several
    // accounts for that endpoint knows which one is wanted.
    match seam
        .resolve(&format!("outbound:{base_url}"), base_url, auth_account)
        .await
    {
        Some(cred) if cred.is_unexpired(now_ms) => {
            let resolver: xai_grok_sampler::SharedBearerResolver = Arc::new(StaticBearerResolver {
                token: cred.token,
                endpoint: base_url.to_owned(),
            });
            (Some(resolver), Some(xai_grok_sampler::AuthScheme::Bearer))
        }
        _ => (None, None),
    }
}

/// Ask a subscribed plugin to mint a fresh credential for a **custom (non-xAI)
/// provider** after a `401`.
///
/// The mirror of [`resolve_custom_provider_auth`] on the recovery side: when a
/// custom endpoint's request comes back `401` and a credential seam is present,
/// drive `refresh_credential`. Returns `true` when the plugin refreshed (the
/// caller then resubmits — the next [`SessionActor::reconstruct_full_config`]
/// re-resolves the now-persisted token via [`resolve_custom_provider_auth`]).
///
/// xAI endpoints are never routed here (their `401` is healed by the session
/// `AuthManager` path), and a missing seam / plugin passthrough returns `false`
/// (fail-open: the `401` then surfaces as before).
///
/// `owner_id` is still `None` — it is not retained across turns, and the plugin
/// owns durable storage of whose token is stale. `auth_account` *is* threaded:
/// it comes from the failed model's catalog entry, so a plugin holding several
/// accounts for this endpoint refreshes the one the model is configured for
/// rather than guessing from `base_url` alone.
async fn refresh_custom_provider_credential(
    seam: Option<Arc<dyn crate::auth::credential_seam::PluginCredentialSeam>>,
    base_url: &str,
    auth_account: Option<&str>,
) -> bool {
    if crate::util::is_xai_api_url(base_url) {
        return false;
    }
    let Some(seam) = seam else {
        return false;
    };
    seam.refresh("outbound_401", None, base_url, auth_account)
        .await
        .is_some()
}

/// Gate inputs bundled with the composed decision so the 401-recovery log can report the components.
#[derive(Clone, Copy)]
struct SessionTokenAuthGate {
    is_session_based: bool,
    model_byok: crate::agent::auth_method::ModelByok,
    /// Whether the session's own credential belongs at the endpoint this
    /// request targets — a first-party host, or the endpoint this deployment
    /// mints its session against. Every BYOK status but `Byok` is gated on it,
    /// so neither an `Unknown` status nor a `[[provider]]` entry's `NotByok`
    /// can put the session bearer on a third-party host.
    endpoint_takes_session_credential: bool,
}

impl SessionTokenAuthGate {
    /// Single place the gate inputs are derived, so all call sites assemble it
    /// identically. `facts` carries the deployment's inference endpoint from
    /// the same memoized config load that produced `byok`.
    fn new(
        auth_method_id: Option<&acp::AuthMethodId>,
        facts: &crate::agent::config::ModelAuthFacts,
        base_url: &str,
    ) -> Self {
        Self {
            // `None` (pre-`authenticate`) classifies as non-session-based, so the gate stays inactive until a method is selected
            is_session_based: auth_method_id
                .is_some_and(crate::agent::auth_method::is_session_based_method),
            model_byok: facts.byok,
            // `is_xai_api_url`, not the stricter bearer form, for the
            // first-party half: a loopback cli-chat-proxy is a first-party
            // route that must keep refreshing.
            endpoint_takes_session_credential: crate::util::is_xai_api_url(base_url)
                || facts.session_inference_base_url.as_deref() == Some(base_url),
        }
    }

    fn active(self) -> bool {
        crate::agent::auth_method::session_token_auth_gate(
            self.is_session_based,
            self.model_byok,
            self.endpoint_takes_session_credential,
        )
    }
}

/// Run a tool call; on an auth-shaped failure, attempt recovery via `AuthManager` and one retry.
/// When `shared_recovery` is `Some`, concurrent 401s in the same batch deduplicate via `OnceCell::get_or_init`.
pub(super) async fn call_with_auth_retry<F, Fut>(
    auth_manager: Option<&std::sync::Arc<crate::auth::AuthManager>>,
    shared_recovery: Option<&tokio::sync::OnceCell<bool>>,
    tool_name: &str,
    mut call: F,
) -> Result<xai_grok_tools::types::output::ToolRunResult, xai_tool_runtime::ToolError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<
            Output = Result<
                xai_grok_tools::types::output::ToolRunResult,
                xai_tool_runtime::ToolError,
            >,
        >,
{
    let result = call().await;
    let Err(ref err) = result else { return result };
    if !is_auth_tool_error(err) {
        return result;
    }
    let Some(am) = auth_manager else {
        return result;
    };
    // Tool-call 401s show up as tool errors, not the ReAuthRequired banner
    let src = crate::auth::recovery::RecoverySource::Background;
    let recovered = match shared_recovery {
        Some(cell) => *cell.get_or_init(|| am.try_recover_unauthorized(src)).await,
        None => am.try_recover_unauthorized(src).await,
    };
    if recovered {
        tracing::info!(
            tool = tool_name,
            "auth recovery: tool 401, recovered, retrying"
        );
        call().await
    } else {
        tracing::warn!(tool = tool_name, "auth recovery: tool 401, refresh failed");
        xai_grok_telemetry::unified_log::warn(
            "auth recovery: tool 401, refresh failed",
            None,
            Some(serde_json::json!({ "tool": tool_name })),
        );
        result
    }
}

const STREAM_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamDrainOutcome {
    Acknowledged,
    TimedOut,
    Revoked,
}

async fn stream_drain_outcome(rx: tokio::sync::oneshot::Receiver<()>) -> StreamDrainOutcome {
    match tokio::time::timeout(STREAM_DRAIN_TIMEOUT, rx).await {
        Ok(Ok(())) => StreamDrainOutcome::Acknowledged,
        Ok(Err(_)) => StreamDrainOutcome::Revoked,
        Err(_) => StreamDrainOutcome::TimedOut,
    }
}

fn error_after_stream_drain(
    outcome: StreamDrainOutcome,
    original: xai_grok_sampler::SamplingErrorInfo,
) -> xai_grok_sampler::SamplingErrorInfo {
    if outcome == StreamDrainOutcome::Revoked {
        revoked_sampling_info()
    } else {
        original
    }
}

fn revoked_sampling_info() -> xai_grok_sampler::SamplingErrorInfo {
    xai_grok_sampler::SamplingErrorInfo {
        kind: xai_grok_sampler::SamplingErrorKind::Api,
        status_code: None,
        message: "sampling result revoked by turn cancellation or rewind".to_string(),
        is_retryable: false,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
        model_metadata: None,
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential: xai_grok_sampling_types::SentCredential::Unknown,
    }
}

/// The `[[model_fallbacks]]` / `provider_error` class for a failed submit.
///
/// [`xai_grok_sampler::classify_error_class`] names these classes, but it reads
/// the rich `SamplingError`, which the turn loop no longer holds: a submit
/// resolves its stream-drain barrier before returning, and that path can only
/// hand back a flattened [`xai_grok_sampler::SamplingErrorInfo`]. The kind, the
/// status and the message it does carry decide the same class for every
/// condition a chain is written against; a transport failure the rich form
/// would have called `stream` lands in `network` here, and an unrecognized
/// class is inert on both sides.
fn failover_error_class(info: &xai_grok_sampler::SamplingErrorInfo) -> &'static str {
    use xai_grok_sampler::SamplingErrorKind;
    if info.status_code == Some(529) || info.message.to_ascii_lowercase().contains("overloaded") {
        return "overloaded";
    }
    match info.kind {
        SamplingErrorKind::RateLimited => "rate_limit",
        SamplingErrorKind::Auth => "auth",
        SamplingErrorKind::Http | SamplingErrorKind::IdleTimeout => "network",
        _ => match info.status_code {
            Some(429) => "rate_limit",
            Some(401) => "auth",
            Some(status) if (500..600).contains(&status) => "5xx",
            _ => "other",
        },
    }
}

impl SessionActor {
    pub(super) async fn prepare_tool_definitions_timed(&self) -> (Vec<ToolDefinition>, u64) {
        let mcp_wait_start = std::time::Instant::now();
        match self.mcp_strategy.get() {
            McpInitStrategy::Blocking => {
                if !self.mcp_state.lock().await.is_initialized() {
                    tracing::info!(
                        "Blocking strategy: waiting for MCP initialization before first prompt..."
                    );
                    self.wait_for_mcp_initialized().await;
                }
            }
            McpInitStrategy::Progressive => {}
        }
        let mcp_wait_ms = mcp_wait_start.elapsed().as_millis() as u64;

        let defs = self.prepare_tool_definitions_inner().await;
        (defs, mcp_wait_ms)
    }

    pub(super) async fn prepare_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.prepare_tool_definitions_timed().await.0
    }

    /// The exact tool specs a turn sends before its structured-output append.
    /// Shared with `SnapshotToolDefinitions` so verbatim mirrors preserve the parent schema.
    pub(crate) fn turn_base_tool_specs(&self, defs: &[ToolDefinition]) -> Vec<ToolSpec> {
        let backend_search_active = self.backend_search_active();
        defs.iter()
            .filter(|td| !backend_search_active || td.function.name != "web_search")
            .cloned()
            .map(ToolSpec::from)
            .collect()
    }

    /// Hosted tools with overrides applied, plus the applied overrides to echo, in one pass.
    fn resolve_hosted(
        &self,
    ) -> (
        Vec<xai_grok_sampling_types::HostedTool>,
        xai_grok_sampling_types::ToolOverrides,
    ) {
        let mut tools = self.agent.borrow().hosted_tools().to_vec();
        let applied = xai_grok_sampling_types::apply_tool_overrides(
            &mut tools,
            self.tool_overrides.borrow().as_ref(),
        );
        (tools, applied)
    }

    /// Ungated. Prefer [`Self::hosted_tools_for_turn`], which folds in the backend-search gate.
    pub(crate) fn effective_hosted_tools(&self) -> Vec<xai_grok_sampling_types::HostedTool> {
        self.resolve_hosted().0
    }

    pub(crate) fn hosted_tools_for_turn(&self) -> Vec<xai_grok_sampling_types::HostedTool> {
        if self.backend_search_active() {
            self.effective_hosted_tools()
        } else {
            Vec::new()
        }
    }

    /// The applied overrides to echo, or `None` when backend search is off.
    pub(crate) fn effective_tool_overrides(
        &self,
    ) -> Option<xai_grok_sampling_types::ToolOverrides> {
        if !self.backend_search_active() {
            return None;
        }
        let applied = self.resolve_hosted().1;
        (!applied.is_empty()).then_some(applied)
    }

    pub(crate) fn backend_search_active(&self) -> bool {
        self.agent.borrow().backend_search_enabled() && self.supports_backend_search.get()
    }

    /// Set the per-turn override and emit it before any turn runs, so a subagent spawned this turn inherits it.
    pub(crate) fn set_tool_overrides(&self, overrides: xai_grok_sampling_types::ToolOverrides) {
        *self.tool_overrides.borrow_mut() = Some(overrides);
        self.emit_resolved_tool_overrides();
    }

    /// Fold a per-turn update at promotion: an object sets, `null` clears to the seed, absent leaves.
    pub(crate) fn apply_tool_overrides_update(
        &self,
        update: Option<xai_grok_sampling_types::ToolOverridesUpdate>,
    ) {
        let Some(update) = update else { return };
        {
            let mut slot = self.tool_overrides.borrow_mut();
            *slot = update.apply(slot.take());
        }
        self.emit_resolved_tool_overrides();
    }

    /// Store this session's cutoff in the cell a subagent spawn reads.
    /// Not gated on backend search, so a bounded parent bounds a searching child even if it isn't searching.
    pub(crate) fn emit_resolved_tool_overrides(&self) {
        let seed = self.agent.borrow().definition().tool_overrides.clone();
        let effective = resolve_configured_cutoff(seed, self.tool_overrides.borrow().as_ref());
        self.resolved_tool_overrides
            .store((!effective.is_empty()).then(|| std::sync::Arc::new(effective)));
    }

    pub(super) async fn prepare_tool_definitions_inner(&self) -> Vec<ToolDefinition> {
        // Clone `ToolBridge` under a *short* `agent` RefCell borrow, then await on the Arc
        // Never hold `self.agent.borrow()` across `.await`
        // Prefire runs `spawn_local` on the same LocalSet as the turn loop, so a parked borrow here would panic if turn/compact/cancel also borrowed it
        let bridge = self.agent.borrow().tool_bridge().clone();

        // Local mode: tool search is always enabled
        let defs = bridge.tool_definitions_builtins_only().await;

        let plan_active = self.plan_mode.lock().is_active();
        filter_cursor_tools_by_plan_mode(defs, plan_active)
    }

    pub(super) fn model_auth_facts(&self, model_id: &str) -> crate::agent::config::ModelAuthFacts {
        self.model_auth_state(model_id).0
    }

    pub(super) fn model_auth_provider(
        &self,
        model_id: &str,
    ) -> Option<crate::auth::AuthProviderRef> {
        self.model_auth_state(model_id).1
    }

    /// Drop the memoized per-model auth state; see [`Self::model_auth_memo`] for why each model/credential chokepoint must call this.
    pub(crate) fn invalidate_model_auth_memo(&self) {
        self.model_auth_memo.replace(None);
    }

    /// Reads and populates [`Self::model_auth_memo`]; a fresh `Unknown` falls back to the last definite entry (see the field's contract).
    fn model_auth_state(
        &self,
        model_id: &str,
    ) -> (
        crate::agent::config::ModelAuthFacts,
        Option<crate::auth::AuthProviderRef>,
    ) {
        use crate::agent::auth_method::ModelByok;
        use crate::session::acp_session::ModelAuthMemo;
        if let Some(memo) = self.model_auth_memo.borrow().as_ref()
            && memo.model_id == model_id
            && memo.facts.byok != ModelByok::Unknown
        {
            return (memo.facts.clone(), memo.provider.clone());
        }
        let (fresh, provider) =
            crate::agent::config::resolve_model_auth_facts_and_provider(model_id);
        if fresh.byok == ModelByok::Unknown {
            if let Some(memo) = self.model_auth_memo.borrow().as_ref()
                && memo.model_id == model_id
            {
                return (memo.facts.clone(), memo.provider.clone());
            }
            return (fresh, provider);
        }
        *self.model_auth_memo.borrow_mut() = Some(ModelAuthMemo {
            model_id: model_id.to_string(),
            facts: fresh.clone(),
            provider: provider.clone(),
        });
        (fresh, provider)
    }

    /// The single writer of a provider mint/rotation into chat-state credentials.
    async fn set_chat_api_key(&self, new_key: String) {
        let mut creds = self.chat_state_handle.get_credentials().await;
        creds.api_key = Some(new_key);
        self.chat_state_handle.update_credentials(creds);
    }

    /// Pre-turn arm for a provider-backed model: mint on a cold cache, re-mint near expiry, and adopt a rotation that chat state missed.
    /// No-op when `current_key` is already the fresh cached token.
    async fn refresh_provider_token_pre_turn(
        &self,
        provider: &crate::auth::AuthProviderRef,
        current_key: Option<&str>,
        model_id: &str,
    ) {
        match provider.ensure_fresh_token(current_key).await {
            crate::auth::ProviderRefreshOutcome::Rotated(new_key) => {
                tracing::info!(
                    model = %model_id,
                    provider = %provider.name,
                    cold = current_key.is_none(),
                    "auth provider token rotated pre-turn"
                );
                self.set_chat_api_key(new_key).await;
            }
            crate::auth::ProviderRefreshOutcome::Unchanged => {}
            // A genuine mint failure; the 401 arm handles the rejection.
            crate::auth::ProviderRefreshOutcome::MintFailed => {
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    provider = %provider.name,
                    model = %model_id,
                    "auth provider pre-turn refresh failed"
                );
                xai_grok_telemetry::unified_log::warn(
                    "auth provider pre-turn refresh failed",
                    Some(self.session_info.id.0.as_ref()),
                    Some(serde_json::json!({
                        "provider": provider.name,
                        "model": model_id,
                        "cold": current_key.is_none(),
                    })),
                );
            }
            // Unusable provider: already warned once, no per-turn breadcrumb.
            crate::auth::ProviderRefreshOutcome::Unusable => {}
        }
    }

    /// 401 arm for a provider-backed model: re-run the helper once and resubmit.
    /// A missing key means the cold mint failed and the request went out unauthenticated, so mint instead.
    /// Returns `false` when the fresh-mint guard blocked the re-run or the helper failed; the 401 then becomes a terminal error.
    async fn try_provider_401_recovery(&self, provider: &crate::auth::AuthProviderRef) -> bool {
        let rejected_key = self.chat_state_handle.get_credentials().await.api_key;
        let recovered = match rejected_key {
            Some(ref rejected_key) => provider.recover_rejected_token(rejected_key).await,
            None => provider.ensure_fresh_token(None).await.rotated(),
        };
        let Some(new_key) = recovered else {
            tracing::warn!(
                session_id = %self.session_info.id.0,
                provider = %provider.name,
                "auth recovery: sampler 401, provider re-mint declined or failed"
            );
            xai_grok_telemetry::unified_log::warn(
                "auth recovery: sampler 401, provider re-mint declined or failed",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({ "provider": provider.name })),
            );
            return false;
        };
        tracing::info!(
            session_id = %self.session_info.id.0,
            provider = %provider.name,
            "auth recovery: sampler 401, auth provider re-mint, retrying"
        );
        xai_grok_telemetry::unified_log::info(
            "auth recovery: sampler 401, auth provider re-mint, retrying",
            Some(self.session_info.id.0.as_ref()),
            None,
        );
        self.set_chat_api_key(new_key).await;
        true
    }

    /// Gate inputs for `model_id` routed to `base_url`.
    /// See [`crate::agent::auth_method::session_token_auth_gate`] for the rationale.
    /// `base_url` keeps an `Unknown` BYOK status refreshable only against first-party xAI hosts.
    fn auth_gate(&self, model_id: &str, base_url: &str) -> SessionTokenAuthGate {
        let facts = self.model_auth_facts(model_id);
        let auth_method = self.auth_method_id.load();
        SessionTokenAuthGate::new(auth_method.as_deref(), &facts, base_url)
    }

    /// Emit a unified-log breadcrumb whenever the session-token refresh gate sees an **`Unknown`** per-model BYOK status on a session-based method.
    /// That condition used to silently demote live sessions to stale-token 401s.
    /// The uploaded per-turn unified log then shows whether the first-party-endpoint fallback kept refresh active or withheld it.
    /// That is visible per session even when server-side metrics only show the aggregate 401.
    /// No-op for a definite `Byok`/`NotByok`, so steady-state turns stay quiet.
    /// A burst of these is itself the signal that `Unknown` is being hit in the field.
    fn log_auth_gate_unknown(&self, site: &str, gate: SessionTokenAuthGate, base_url: &str) {
        use crate::agent::auth_method::ModelByok;
        if gate.model_byok != ModelByok::Unknown || !gate.is_session_based {
            return;
        }
        let refresh_active = gate.active();
        let ctx = serde_json::json!({
            "site": site,
            "model_byok": gate.model_byok.as_str(),
            "is_session_based": gate.is_session_based,
            "endpoint_takes_session_credential": gate.endpoint_takes_session_credential,
            "refresh_active": refresh_active,
            "base_url": base_url,
        });
        let sid = Some(self.session_info.id.0.as_ref());
        if refresh_active {
            xai_grok_telemetry::unified_log::info(
                "auth gate: Unknown BYOK on first-party endpoint — session-token refresh kept active",
                sid,
                Some(ctx),
            );
        } else {
            xai_grok_telemetry::unified_log::warn(
                "auth gate: Unknown BYOK on non-first-party endpoint — refresh withheld (may surface stale-token 401)",
                sid,
                Some(ctx),
            );
        }
    }

    /// Reconstruct a full `SamplerConfig` (with credentials) by combining the actor's `SamplingConfig` and `Credentials`.
    /// Folds in the URL-derived headers (cli-chat-proxy auth, the staging auth header) so the sampler crate stays URL-agnostic.
    pub(super) async fn reconstruct_full_config(&self) -> SamplingConfig {
        #[allow(clippy::items_after_statements)]
        #[derive(Debug)]
        struct TraceContextInjector;
        impl xai_grok_sampler::HeaderInjector for TraceContextInjector {
            fn inject(&self, headers: &mut reqwest::header::HeaderMap) {
                if let Some(tp) = xai_file_utils::trace_context::current_traceparent()
                    && let Ok(v) = reqwest::header::HeaderValue::from_str(&tp)
                {
                    headers.insert("traceparent", v);
                }
            }
        }

        let cfg = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .unwrap_or_else(|| xai_grok_sampling_types::SamplingConfig {
                base_url: String::new(),
                model: String::new(),
                max_completion_tokens: None,
                temperature: None,
                top_p: None,
                api_backend: Default::default(),
                extra_headers: Default::default(),
                query_params: Default::default(),
                env_http_headers: Default::default(),
                context_window: std::num::NonZeroU64::new(256_000).unwrap(),
                reasoning_effort: None,
                stream_tool_calls: None,
            });
        let creds = self.chat_state_handle.get_credentials().await;
        let model_facts = self.model_auth_facts(cfg.model.as_str());
        // Gate on the stable session classifier, not `creds.auth_type`; see `crate::agent::auth_method::session_token_auth_gate`
        // `cfg.base_url` keeps an `Unknown` BYOK status refreshable against first-party xAI hosts
        // That avoids leaking the session token to a third-party endpoint
        let auth_method = self.auth_method_id.load();
        let gate = SessionTokenAuthGate::new(auth_method.as_deref(), &model_facts, &cfg.base_url);
        let use_bearer_resolver = gate.active();
        self.log_auth_gate_unknown("reconstruct_full_config", gate, &cfg.base_url);
        // Refresh the session token before the sampler reads it; gated to sessions that use it.
        if use_bearer_resolver && let Some(am) = self.auth_manager.as_ref() {
            let _ = am.auth().await;
        }
        // Session path: only seed a wire-valid AT
        // Hard-expired keys must not land in default headers when the resolver has nothing to stamp
        let api_key = if use_bearer_resolver {
            // `use_bearer_resolver` means the endpoint is a first-party xAI URL.
            // A session from another authority must not seed the default headers either.
            self.auth_manager
                .as_ref()
                .filter(|_| ActiveAuthBackend::default().is_xai_authority())
                .and_then(|am| am.current_wire_valid().map(|a| a.key))
        } else {
            creds.api_key
        };
        let mut auth_scheme = model_facts.auth_scheme;
        // Auth attachment splits cleanly by provider:
        // - xAI session-based auth (`use_bearer_resolver`) keeps the live
        //   `AuthManager` token via `WireValidBearerResolver`;
        // - every other endpoint is a custom/BYOK provider, where a subscribed
        //   plugin may supply the outbound bearer (`resolve_custom_provider_auth`
        //   self-guards xAI URLs and overrides `auth_scheme` to Bearer on a hit);
        // - with no live resolver on either arm, the sampler falls back to the
        //   construction-time `api_key` header exactly as before.
        let bearer_resolver: Option<xai_grok_sampler::SharedBearerResolver> = if use_bearer_resolver
        {
            self.auth_manager.as_ref().map(|am| {
                crate::auth::credential_provider::WireValidBearerResolver::shared(am.clone())
            })
        } else {
            let (resolver, scheme_override) = resolve_custom_provider_auth(
                self.build_credential_seam(),
                &cfg.base_url,
                model_facts.auth_account.as_deref(),
            )
            .await;
            if let Some(scheme) = scheme_override {
                auth_scheme = scheme;
            }
            resolver
        };
        let mut extra_headers = cfg.extra_headers;
        crate::agent::config::inject_url_derived_headers(
            &mut extra_headers,
            creds.alpha_test_key.as_deref(),
            &cfg.base_url,
        );
        let compaction_at_tokens = self.compaction_at_tokens.get();
        let compactions_remaining = self.compactions_remaining.get();
        if compactions_remaining.is_some() || compaction_at_tokens.is_some() {
            let has_compaction_summary = self
                .chat_state_handle
                .get_last_compaction_prompt_index()
                .await
                .is_some();
            if let Some(value) =
                compactions_remaining.and_then(|c| c.resolve(has_compaction_summary))
            {
                extra_headers.insert("x-compactions-remaining".to_string(), value.to_string());
            }
            // Send on the uncompacted prefix; drop the header once the session compacts.
            if !has_compaction_summary
                && let Some(value) = compaction_at_tokens.and_then(|c| {
                    c.resolve(
                        cfg.context_window.get(),
                        self.compaction.threshold_percent.get(),
                    )
                })
            {
                extra_headers.insert("x-compaction-at".to_string(), value.to_string());
            }
        }
        let extra_response_includes = crate::agent::config::response_include_extensions(
            self.supports_backend_search.get(),
            &cfg.api_backend,
            &cfg.base_url,
        );
        SamplingConfig {
            api_key,
            base_url: cfg.base_url,
            model: cfg.model,
            max_completion_tokens: cfg.max_completion_tokens,
            temperature: cfg.temperature,
            top_p: cfg.top_p,
            api_backend: cfg.api_backend,
            auth_scheme,
            extra_headers,
            extra_response_includes,
            query_params: cfg.query_params.clone(),
            env_http_headers: cfg.env_http_headers.clone(),
            context_window: cfg.context_window.get(),
            proxy: model_facts.proxy.clone(),
            client_version: creds.client_version,
            reasoning_effort: cfg.reasoning_effort,
            thinking: model_facts.thinking,
            max_concurrent: model_facts.max_concurrent,
            // The turn the user is watching; aux one-shots resolved elsewhere
            // declare themselves background.
            concurrency_class: xai_grok_sampler::ConcurrencyClass::Interactive,
            force_http1: false,
            max_retries: Some(self.max_retries),
            stream_tool_calls: cfg.stream_tool_calls.unwrap_or(false),
            idle_timeout_secs: None,
            client_identifier: self.client_identifier.clone(),
            deployment_id: crate::managed_config::resolve_deployment_id(
                crate::managed_config::resolve_deployment_key().as_deref(),
            ),
            user_id: self
                .auth_manager
                .as_ref()
                .and_then(|am| am.current_or_expired())
                .filter(|a| a.is_xai_auth())
                .map(|a| a.user_id),
            origin_client: self.origin_client.clone(),
            // Attribute sampler 401s against the bearer sent on the wire.
            // `None` for sessions spawned without an `AuthManager` (BYOK direct, certain test fixtures)
            attribution_callback: self.attribution_callback.clone(),
            bearer_resolver,
            supports_backend_search: self.supports_backend_search.get(),
            compactions_remaining: self.compactions_remaining.get(),
            compaction_at_tokens: self.compaction_at_tokens.get(),
            // The sampler sends the opt-in header itself when this is set.
            doom_loop_recovery: self.doom_loop_recovery,
            header_injector: Some(std::sync::Arc::new(TraceContextInjector)),
            // Attach the `provider_request` interceptor only when a hook is
            // subscribed, keeping the hot path free of a body-serialization
            // round-trip otherwise. Built-in failover consults `provider_error`
            // directly from the turn loop, so the sampler's error hook stays
            // unset here.
            request_interceptor: self.build_hook_request_interceptor().await,
            error_hook: None,
        }
    }

    /// Install the auto-mode permission classifier with a live LLM side-query.
    /// It follows the laziness-classifier pattern: `prepare_chat_completion` and `conversation_collect` run on a LocalSet task.
    /// A channel bridges the `Send` permission actor.
    /// The heuristic runs only when the side-query errors or returns unparseable text.
    pub(crate) async fn wire_permission_auto_llm_classifier(self: &Arc<Self>) {
        // `is_auto_mode()` is only true when the gated `set_auto_mode` call sites (spawn and SetAutoMode) flipped the permission manager to auto
        // This guard already prevents wiring whenever the feature gate is off
        if !self.permissions.is_auto_mode() {
            return;
        }
        // Idempotency: if a side-query worker is already wired, don't spawn another.
        // Reconnect (load_session) and repeated SetAutoMode { enabled: true } can call this again
        // Re-wiring would leak the prior spawn_local worker blocked on a dropped receiver while a new task handles requests
        if self.permissions.has_llm_side_query() {
            return;
        }
        // Resolve the `[auto_mode]` config (local config, then remote, then the built-in default)
        // When auto mode is enabled and unconfigured, the classifier uses the current model at low reasoning effort (if the model supports it)
        // It uses the `just_command` prompt; local config and remote settings override these
        let auto_cfg = crate::util::config::resolve_auto_mode_config_from_disk();
        let session_sampling = self.chat_state_handle.get_sampling_config().await;
        let session_model = session_sampling
            .as_ref()
            .map(|c| c.model.clone())
            .unwrap_or_default();
        let session_context_window = session_sampling.map(|c| c.context_window.get());
        // The configured value is a catalog key and is passed on as one: the
        // resolver looks the key up before it falls back to a bare-slug scan, so
        // a pin qualified to one of two providers serving the same slug keeps
        // naming that provider's entry, endpoint and credential.
        let classifier_pin = auto_cfg.classifier_model.as_deref();
        // The classifier gates a tool call, so it competes for slots as
        // interactive work rather than taking the background lane.
        let aux_classifier_sampler = match classifier_pin {
            Some(key) => self.resolve_aux_route(key, false).await,
            None => None,
        };
        // Window the classifier request has to fit in: the pinned entry's own
        // when the pin routed, else the session's. Unknown on both ends fails
        // open — the request is issued and the endpoint rejects it, which is
        // the behaviour this check exists to pre-empt, not to replace.
        let classifier_context_window = match classifier_pin {
            Some(key) if aux_classifier_sampler.is_some() => self
                .resolve_aux_sampler_config(key)
                .await
                .map(|cfg| cfg.context_window),
            _ => None,
        }
        .or(session_context_window)
        .unwrap_or(u64::MAX);
        let models = self.models_manager.models();
        // Ask the capability question with the same key, not with the routing
        // slug the resolver came back with: re-resolving that slug can land on a
        // different entry, and then the reasoning-effort default would be taken
        // from a model the request is not going to.
        let effective_supports_re = crate::agent::config::effective_classifier_supports_re(
            classifier_pin.filter(|_| aux_classifier_sampler.is_some()),
            &session_model,
            &models,
        );
        let (prompt_type, classifier_reasoning_effort) =
            crate::util::config::auto_mode_classifier_defaults(&auto_cfg, effective_supports_re);
        let classify_timeout = crate::util::config::auto_mode_classify_timeout(&auto_cfg);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(
            Vec<xai_grok_workspace::permission::ClassifierMessage>,
            tokio::sync::oneshot::Sender<
                Result<String, xai_grok_workspace::permission::ClassifierFailure>,
            >,
        )>();
        let session = Arc::clone(self);
        // One shared worker serializes parent and subagent classifier requests.
        tokio::task::spawn_local(async move {
            while let Some((messages, respond_to)) = rx.recv().await {
                let request_span = region!("permission.classifier_request", Parent::Root);
                let result = async {
                    let route = match &aux_classifier_sampler {
                        Some(route) => route.clone(),
                        // No pin, or a pin with no route of its own: the
                        // side-query rides the session's own client and model. A
                        // chain written against the session model still applies,
                        // so the route is described the same way rather than
                        // opting out of failover.
                        None => session.session_client_aux_route(false).await.map_err(|e| {
                            xai_grok_workspace::permission::ClassifierFailure::TransportError(
                                e.to_string(),
                            )
                        })?,
                    };
                    let session_id = session.session_info.id.to_string();
                    let backend = route.client.api_backend();
                    let request = build_permission_classifier_request(
                        &backend,
                        route.wire_model.clone(),
                        messages,
                        classifier_reasoning_effort,
                        session_id,
                    );
                    // A request that cannot fit is refused before it is sent:
                    // the endpoint's rejection costs a round-trip on the
                    // critical path of every tool call, and the heuristic is
                    // the floor either way.
                    let input_tokens = xai_chat_state::estimate_conversation_tokens(&request.items);
                    if !classifier_request_fits_context(input_tokens, classifier_context_window) {
                        return Err(
                            xai_grok_workspace::permission::ClassifierFailure::TransportError(
                                "permission auto classifier request exceeds context window"
                                    .to_owned(),
                            ),
                        );
                    }
                    // Which shape the verdict schema rode in. A verdict that was
                    // never schema-constrained parses by luck, so the mechanism
                    // has to be readable next to the outcome.
                    tracing::debug!(
                        backend = ?backend,
                        schema_mechanism = if request.tools.is_empty() {
                            "native_response_schema"
                        } else {
                            "structured_output_tool"
                        },
                        "permission auto classifier request built"
                    );
                    // One alternative on a runtime failure, no more. This query
                    // is issued before every tool call the session makes, so its
                    // budget is paid on the critical path of the whole session;
                    // `classify_timeout` still bounds the hop and the original
                    // attempt together, and the heuristic remains the floor.
                    let fut = session.aux_collect_with_fallback(
                        "permission_classifier",
                        route,
                        request,
                        AUX_HOPS_PERMISSION_CLASSIFIER,
                    );
                    let response = tokio::time::timeout(classify_timeout, fut)
                        .await
                        .map_err(|_| xai_grok_workspace::permission::ClassifierFailure::Timeout)?
                        .map_err(|e| {
                            xai_grok_workspace::permission::ClassifierFailure::TransportError(
                                e.to_string(),
                            )
                        })?;
                    Ok(classifier_verdict_text(&response))
                }
                .await;
                if let Err(error) = &result {
                    tracing::warn!(%error, "permission auto classifier side-query failed");
                }
                request_span.close();
                let _ = respond_to.send(result);
            }
        });
        let clf =
            xai_grok_workspace::permission::LlmPermissionClassifier::with_channel(tx, prompt_type);
        debug_assert!(
            clf.has_side_query(),
            "channel-wired classifier must report has_side_query"
        );
        self.permissions.set_classifier_with_side_query(clf, true);
        tracing::info!(
            session_id = %self.session_info.id,
            "Wired live LLM permission auto-mode classifier (session sampling channel)"
        );
    }

    /// Resolve a standalone aux-model `SamplerConfig` for `slug` via the shared catalog routing, gathering the session-local auth context once.
    /// The routing is Tier-1 catalog creds / Tier-2 xAI-proxy via session token / `XAI_API_KEY` / deployment key.
    /// Shared by image-describe and the classifier so the gather can't drift.
    /// `None` means the caller falls back to the session model.
    pub(super) async fn resolve_aux_sampler_config(
        &self,
        slug: &str,
    ) -> Option<xai_grok_sampler::SamplerConfig> {
        let creds = self.chat_state_handle.get_credentials().await;
        // The endpoint the active session model talks to. Gates the resolver's
        // first-party fallback tier so a session on a custom `[[provider]]`
        // cannot have an aux call rerouted to xAI.
        let session_base_url = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.base_url)
            .unwrap_or_default();
        let session_key = self
            .auth_manager
            .as_ref()
            .and_then(|am| am.current_or_expired().map(|a| a.key.clone()));
        let models = self.models_manager.models();
        let endpoints = self.models_manager.endpoints();
        let disable_api_key_auth = self
            .auth_manager
            .as_ref()
            .map(|am| am.grok_com_config().api_key_auth_disabled())
            .unwrap_or(false);
        crate::agent::config::resolve_aux_model_sampling_config(
            slug,
            &models,
            &endpoints,
            &session_base_url,
            session_key.as_deref(),
            disable_api_key_auth,
            creds.alpha_test_key.clone(),
            creds.client_version.clone(),
        )
    }
    /// Gather this session's side of
    /// [`crate::agent::config::aux_config_on_session_credential`] — the second
    /// chance for an aux slug the catalog resolver refused a credential.
    async fn aux_config_on_session_credential(
        &self,
        slug: &str,
        active_session_config: &xai_grok_sampler::SamplerConfig,
    ) -> Option<xai_grok_sampler::SamplerConfig> {
        let creds = self.chat_state_handle.get_credentials().await;
        let models = self.models_manager.models();
        let cfg = crate::agent::config::aux_config_on_session_credential(
            slug,
            &models,
            active_session_config,
            creds.alpha_test_key.clone(),
            creds.client_version.clone(),
        )?;
        tracing::debug!(
            aux_model = %slug,
            base_url = %cfg.base_url,
            "aux model carries no credential of its own; routing it on the session's own \
             provider credential, which was minted for that same endpoint"
        );
        Some(cfg)
    }
    /// Resolve a dedicated sampler client plus the wire model for an auxiliary
    /// one-shot model `slug` (Auto-mode classifier, next-prompt suggestion, ...),
    /// stamping session-local auth/attribution like image-describe (which relies
    /// on the resolver, not a config override, for `base_url`/`api_backend` so
    /// credentials stay consistent).
    ///
    /// The returned model is the resolved entry's own routing slug and the
    /// returned client points at that entry's endpoint, so the two cannot
    /// disagree. That pairing is the whole point of going through here: `slug`
    /// may live on a different `[[provider]]` than the session runs on, and a
    /// client built for provider A's endpoint and credential cannot serve
    /// provider B's slug — the vendor rejects it by name.
    ///
    /// `None` ⇒ the aux model has no route of its own; each caller decides
    /// whether to degrade to the session client + model or to skip the call.
    pub(super) async fn resolve_aux_sampler_client(
        &self,
        slug: &str,
    ) -> Option<(xai_grok_sampler::SamplingClient, String, u64)> {
        let active_session_config = self.reconstruct_full_config().await;
        let mut cfg = match self.resolve_aux_sampler_config(slug).await {
            Some(cfg) => cfg,
            // The catalog refused this entry a credential. It is still routable
            // when the session's own credential was minted for its endpoint.
            None => {
                self.aux_config_on_session_credential(slug, &active_session_config)
                    .await?
            }
        };
        crate::agent::config::stamp_session_local_sampler_fields(
            &mut cfg,
            &active_session_config,
            self.client_identifier.clone(),
            Some(self.max_retries),
        );
        let model = cfg.model.clone();
        let context_window = cfg.context_window;
        let client = xai_grok_sampler::SamplingClient::new(cfg)
            .map_err(|e| {
                tracing::warn!(error = %e, aux_model = %slug, "aux sampler build failed; caller falls back")
            })
            .ok()?;
        Some((client, model, context_window))
    }

    #[tracing::instrument(
        name = "session.prepare_chat_completion",
        skip_all,
        fields(force_http1)
    )]
    pub(super) async fn prepare_chat_completion(
        &self,
        force_http1: bool,
    ) -> Result<xai_grok_sampler::SamplingClient, acp::Error> {
        // Check if the JWT token is expired/near-expiration and refresh from config if needed
        self.refresh_token_if_expired().await;

        let mut full_config = self.reconstruct_full_config().await;
        full_config.force_http1 = force_http1;
        // Captured before the config is consumed: an auth failure's message has
        // to name the credential the *endpoint this client would call* checks,
        // which is not the session's credential when the model is a custom
        // provider's.
        let (model, base_url) = (full_config.model.clone(), full_config.base_url.clone());
        let sampling_client = xai_grok_sampler::SamplingClient::new(full_config)
            .map_err(|e| self.to_acp_error(e, &model, &base_url))?;
        Ok(sampling_client)
    }

    // -----------------------------------------------------------------
    // Sampler-driven turn: per-turn config refresh and failure recovery
    // -----------------------------------------------------------------

    // (See `SamplerFailureRecovery` enum near the bottom of the impl block.)

    /// Refresh auth and push a fresh `SamplerConfig` before each turn.
    pub(crate) async fn prepare_sampler_for_turn(&self) {
        self.refresh_token_if_expired().await;
        let mut sampler_config = self.reconstruct_full_config().await;
        if self.tool_context.task_output_token_budget.is_some()
            || self.tool_context.sampler_retry_only_before_output
        {
            sampler_config.doom_loop_recovery = None;
        }
        // Carry over the session's per-chunk idle timeout via `SamplerConfig.idle_timeout_secs`
        sampler_config.idle_timeout_secs = Some(self.inference_idle_timeout.as_secs());
        self.sampler_handle.update_config(sampler_config);
    }
    /// Classify a terminal auth failure and give it its call-to-action.
    ///
    /// Which credential failed is decided by the endpoint, not by the code
    /// path that noticed. [`AuthRemedy`](crate::auth::AuthRemedy) speaks only
    /// for the `AuthManager`'s credential — the xAI session or the operator's
    /// auth provider — so on a third-party endpoint every one of its arms is
    /// about a credential the request never carried. `SelfHealing` is the
    /// worst of them there: it downgrades the failure to `auth_transient`,
    /// suppressing the client's re-auth banner entirely, and promises a
    /// recovery that a wrong provider key will never make.
    ///
    /// `remedy` is `None` for a session with no `AuthManager` at all, which
    /// has no first-party credential to speak for either. An unreadable
    /// sampling config falls back to the first-party path: that is both the
    /// pre-existing behavior and the default the ACP `meta.firstParty` flag
    /// already takes, and it beats a provider message naming no provider.
    async fn classify_auth_failure(
        &self,
        remedy: Option<crate::auth::AuthRemedy>,
        message: String,
        status_code: Option<u16>,
    ) -> (&'static str, String) {
        let endpoint = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .filter(|cfg| !crate::util::is_xai_api_bearer_url(&cfg.base_url));
        let Some(cfg) = endpoint else {
            return match remedy {
                Some(remedy) => self.apply_auth_remedy(&remedy, message, status_code),
                None => ("auth", message),
            };
        };
        let advice = crate::sampling::error::provider_auth_failed_advice(&cfg.model, &cfg.base_url);
        xai_grok_telemetry::unified_log::info(
            "auth: turn failure on a third-party endpoint",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "status_code": status_code,
                "model": cfg.model,
            })),
        );
        ("auth", format!("{message}\n\n{advice}"))
    }
    /// Fold an auth remedy into a turn failure: its advice becomes the tail of
    /// the message, and its `turn_error_type` the classification the client
    /// keys its re-auth prompt off.
    fn apply_auth_remedy(
        &self,
        remedy: &crate::auth::AuthRemedy,
        message: String,
        status_code: Option<u16>,
    ) -> (&'static str, String) {
        xai_grok_telemetry::unified_log::info(
            "auth: turn failure classified",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "status_code": status_code,
                "remedy": format!("{remedy:?}"),
            })),
        );
        let message = match remedy.advice() {
            Some(advice) => format!("{message}\n\n{advice}"),
            None => message,
        };
        (remedy.turn_error_type(), message)
    }

    /// Terminal failure for a turn the auth-retry budget gave up on, the one terminal path that lives outside [`Self::handle_sampling_failure`].
    ///
    /// Every terminal path owes the client one `RetryState::Failed`: it is what raises the pager's re-auth prompt and its turn-failed block.
    /// This arm used to return its `acp::Error` without one, so a turn that died on repeated 401s ended in silence.
    /// Terminal failure when consecutive Length salvages exceed the cap.
    /// Executing more calls is not converging, so report the pre-salvage `MaxTokensTruncation` failure.
    /// The sampler emitted only Completed events for these samples, so the error signal is recorded here.
    pub(crate) async fn fail_turn_length_salvage_exhausted(&self) -> acp::Error {
        let kind = xai_grok_sampler::SamplingErrorKind::MaxTokensTruncation;
        let message = xai_grok_sampling_types::SamplingError::MaxTokensTruncation.to_string();
        self.signals_handle().record_error_typed(kind.as_str());
        self.log_terminal_failure(kind.as_str(), None, &message);
        self.send_xai_notification(XaiSessionUpdate::RetryState(
            crate::extensions::notification::RetryState::Failed {
                error_type: kind.as_str().to_owned(),
                message: message.clone(),
            },
        ))
        .await;
        acp::Error::internal_error().data(crate::sampling::error::terminal_error_data(
            message, None, kind,
        ))
    }

    pub(crate) async fn fail_turn_auth_budget_exhausted(&self, message: String) -> acp::Error {
        const STATUS: Option<u16> = Some(401);
        let remedy = self
            .auth_manager
            .as_ref()
            .map(|am| am.auth_remedy().after_retries_exhausted());
        let (error_type, message) = self.classify_auth_failure(remedy, message, STATUS).await;
        self.log_terminal_failure(error_type, STATUS, &message);
        self.send_xai_notification(XaiSessionUpdate::RetryState(
            crate::extensions::notification::RetryState::Failed {
                error_type: error_type.to_owned(),
                message: message.clone(),
            },
        ))
        .await;
        acp::Error::internal_error().data(crate::sampling::error::error_data_with_status(
            message, STATUS,
        ))
    }

    fn log_terminal_failure(&self, error_type: &str, status_code: Option<u16>, message: &str) {
        let auth = self
            .auth_manager
            .as_ref()
            .and_then(|am| am.current_or_expired());
        let reauthable = is_reauthable_failure(Some(error_type), message);
        xai_grok_telemetry::unified_log::warn(
            "turn.terminal_failure",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "error_type": error_type,
                "status_code": status_code,
                "reauthable": reauthable,
                "auth_mode": auth.as_ref().map(|a| format!("{:?}", a.auth_mode)),
                "key_prefix": auth.as_ref().map(|a| xai_grok_auth::bearer_suffix(&a.key).to_owned()),
                "expires_at": auth
                    .as_ref()
                    .and_then(|a| a.expires_at.map(|e| e.to_rfc3339())),
                "message": crate::util::truncate(message, 300),
            })),
        );
    }
    /// Repair the history behind an encrypted-content 400 and report how many
    /// reasoning items it cost.
    ///
    /// Zero means there was nothing to repair, and the caller must not
    /// resubmit: the rebuilt request would be byte-identical and earn the
    /// same rejection. Anything above zero means the next turn attempt sends
    /// a strictly smaller history, so the recovery cannot cycle.
    ///
    /// `replace_conversation` is fire-and-forget, but the actor serializes its
    /// commands, so the `build_request` the resubmit issues is processed after
    /// this replacement lands.
    pub(crate) async fn repair_unverifiable_reasoning(&self, message: &str) -> usize {
        let named_id = xai_grok_sampling_types::encrypted_content_item_id(message);
        let mut items = self.chat_state_handle.get_conversation().await;
        let dropped = xai_grok_sampling_types::drop_unverifiable_reasoning(&mut items, named_id);
        if dropped == 0 {
            return 0;
        }
        self.chat_state_handle.replace_conversation(items);
        tracing::warn!(
            dropped,
            named_item = named_id.unwrap_or("<unnamed>"),
            "encrypted reasoning rejected by the endpoint; dropped it from the history"
        );
        xai_grok_telemetry::unified_log::warn(
            "turn.encrypted_reasoning_dropped",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "dropped": dropped,
                "named_item": named_id.is_some(),
            })),
        );
        self.send_xai_notification(XaiSessionUpdate::RetryState(
            crate::extensions::notification::RetryState::Retrying {
                attempt: 1,
                max_retries: 1,
                reason: "The model's earlier reasoning cannot be verified by this endpoint; \
                         dropping it and retrying"
                    .to_string(),
                // Same tag the terminal `RetryState::Failed` on this path
                // sends, so the client classifies the retry notice and the
                // give-up notice as one story instead of text-sniffing the
                // reason.
                error_type: Some("encrypted_content_mismatch".to_string()),
            },
        ))
        .await;
        dropped
    }

    /// The failed request's usage never arrived: fail the task budget closed (budgeted children) or mark the session totals incomplete.
    /// Send-only, not spawned: the mark must be in the actor's mailbox before the completing turn's billing epilogue reads the ledger.
    /// It must also land before a queued prompt promotes and resets the ledger.
    fn mark_turn_usage_unaccounted(self: &Arc<Self>) {
        if self.tool_context.task_output_token_budget.is_some() {
            self.tool_context.fail_task_output_usage_closed();
        } else {
            self.chat_state_handle
                .mark_usage_incomplete_nowait(true, true);
        }
    }

    /// Classify a terminal sampler failure and decide recovery.
    /// `transient`: turn-loop retry state (the loop owns the counters).
    pub(crate) async fn handle_sampling_failure(
        self: &Arc<Self>,
        error: xai_grok_sampler::SamplingErrorInfo,
        rate_limit_waits: u32,
        transient: TransientRetryState,
        mid_salvage_continuation: bool,
    ) -> Result<SamplerFailureRecovery, acp::Error> {
        use xai_grok_sampler::SamplingErrorKind;

        // On an in-flight salvage continuation, a max-tokens failure or a probable context overflow is not terminal
        // For an overflow, compact-and-resubmit would delete the continue reminder and split the report
        // The turn loop completes the turn truncated with the committed segments
        // Return the typed error quietly: no failure UX, no terminal telemetry
        // Return before the budgeted-child rewrites so the marker survives for workflow children
        // The request's usage never arrived; account for it fail-closed
        //
        // Overflow is the server's own context-length message, or the estimate-over-window signal
        // The estimate is checked without the compaction suppression gate: suppression must not turn an overflowed continuation into a hard failure
        // The estimate heuristic never fires for kinds naming a non-overflow cause
        // Auth failures refresh-and-resubmit below (the resubmit IS the continuation; budgeted children instead fail their grant closed)
        // Rate limits keep their terminal notification, and the deterministic encrypted-content 400 keeps its friendly arm
        let encrypted_content_mismatch = matches!(error.kind, SamplingErrorKind::Api)
            && error.status_code == Some(400)
            && error.message.contains("encrypted_content");
        let quiet_mid_salvage = mid_salvage_continuation
            && (error.kind == SamplingErrorKind::MaxTokensTruncation
                || xai_grok_sampling_types::is_context_length_error(&error.message)
                || (!matches!(
                    error.kind,
                    SamplingErrorKind::Auth | SamplingErrorKind::RateLimited
                ) && !encrypted_content_mismatch
                    && self.estimate_exceeds_error_context_window(&error).await));
        if quiet_mid_salvage {
            self.mark_turn_usage_unaccounted();
            let mut data = crate::sampling::error::terminal_error_data(
                error.message,
                error.status_code,
                SamplingErrorKind::MaxTokensTruncation,
            );
            // Two populations for rollout sizing: either the continuation was unsalvageable at the cap, or the request no longer fit
            // Unsalvageable means empty or a truncated tool-call tail; the sampler folds both into `MaxTokensTruncation`
            // `terminal_error_data` returns the object shape for `MaxTokensTruncation`
            // Guard rather than index-assign so a shape change cannot panic here
            if let Some(obj) = data.as_object_mut() {
                obj.insert(
                    crate::sampling::error::SALVAGE_CAUSE_KEY.to_owned(),
                    serde_json::json!(if error.kind == SamplingErrorKind::MaxTokensTruncation {
                        crate::sampling::error::SALVAGE_CAUSE_EMPTY
                    } else {
                        crate::sampling::error::SALVAGE_CAUSE_OVERFLOW
                    }),
                );
            }
            return Err(acp::Error::internal_error().data(data));
        }

        if self.tool_context.task_output_token_budget.is_some() {
            self.tool_context.fail_task_output_usage_closed();
            let message = format!(
                "budgeted workflow child model request failed; output grant exhausted: {}",
                error.message
            );
            self.log_terminal_failure("output_budget_usage_unknown", error.status_code, &message);
            return Err(acp::Error::internal_error().data(message));
        }
        if self.tool_context.sampler_retry_only_before_output {
            self.mark_turn_usage_unaccounted();
            let message = format!(
                "workflow child model request failed; usage may understate real spend: {}",
                error.message
            );
            self.log_terminal_failure(
                "workflow_child_sampling_failed",
                error.status_code,
                &message,
            );
            return Err(acp::Error::internal_error().data(message));
        }

        // Never compact mid-salvage: the rewrite would drop the continue reminder and split the joined report
        // Genuine overflows already completed truncated in the quiet arm above
        // The remaining mid-salvage kinds (rate limit) take their terminal arms below
        if !mid_salvage_continuation && self.should_compact_on_error(&error).await {
            // SAFETY: `should_compact_on_error` returned true only when `model_metadata.context_window` was Some(>0)
            let cw = error
                .model_metadata
                .as_ref()
                .and_then(|m| m.context_window)
                .expect("should_compact_on_error guarantees context_window");
            {
                let total_tokens = self.chat_state_handle.get_estimated_total_tokens().await;
                let percentage = xai_token_estimation::usage_percentage_u8(total_tokens, cw);

                // Update the in-memory sampling config's `context_window` if the model reported a different value (mirror the legacy path's bookkeeping)
                if let Some(mut cfg) = self.chat_state_handle.get_sampling_config().await
                    && let Some(new_cw) = std::num::NonZeroU64::new(cw)
                    && self.compaction.context_window_override.is_none()
                {
                    cfg.context_window = new_cw;
                    self.chat_state_handle.update_sampling_config(cfg);
                }

                let trigger_info = compaction::AutoCompactTriggerInfo {
                    tokens_used: total_tokens,
                    context_window: cw,
                    percentage,
                };
                if let Err(e) = self.run_compact_only(trigger_info, false).await {
                    if Self::is_auth_compact_error(&e) {
                        return Err(self.surface_compact_auth_failure(e).await);
                    }
                    return Err(e);
                }
                return Ok(SamplerFailureRecovery::CompactAndResubmit);
            }
        }

        // Telemetry and notification for terminal failures
        // The drainer already recorded `record_error_typed` from the `SamplingEvent::Failed` event
        // Here we send the `RetryState::Failed` notification, which the drainer intentionally skips because it would fire mid-retry
        let detailed_message = error.message.clone();

        // 2. Encrypted-content mismatch: friendly error, no retry.
        //    Detect via the BadRequest and "encrypted_content" message pattern that `SamplingError::is_encrypted_content_error` used in the legacy path
        if encrypted_content_mismatch {
            self.signals_handle()
                .record_error_typed("encrypted_content_mismatch");
            if self.repair_unverifiable_reasoning(&error.message).await > 0 {
                return Ok(SamplerFailureRecovery::ResubmitWithoutReasoning);
            }
            // Nothing left to drop: the rejection is about content this shell
            // cannot find in the history, so resubmitting would repeat it.
            let friendly = "This session's conversation history is incompatible \
                            with the current model. Please start a new session."
                .to_string();
            self.log_terminal_failure("encrypted_content_mismatch", error.status_code, &friendly);
            self.send_xai_notification(XaiSessionUpdate::RetryState(
                crate::extensions::notification::RetryState::Failed {
                    error_type: "encrypted_content_mismatch".to_string(),
                    message: friendly.clone(),
                },
            ))
            .await;
            return Err(acp::Error::invalid_params().data(friendly));
        }

        if matches!(error.kind, SamplingErrorKind::RateLimited) {
            self.log_terminal_failure("rate_limited", error.status_code, &detailed_message);
            self.send_xai_notification(XaiSessionUpdate::RetryState(
                crate::extensions::notification::RetryState::Exhausted {
                    attempts: rate_limit_waits,
                    reason: detailed_message.clone(),
                    is_rate_limited: true,
                },
            ))
            .await;
            let acp_err = acp::Error::new(
                crate::sampling::error::RATE_LIMITED_ERROR_CODE,
                "Rate limited".to_string(),
            )
            .data(detailed_message);
            return Err(acp_err);
        }

        // 4. Auth-401 recovery only applies to refreshable session-token auth (the stable gate, not `creds.auth_type`).
        //    A static api-key isn't refreshable, so retrying re-sends the same rejected bearer and 401-loops the turn
        //    See `crate::agent::auth_method::session_token_auth_gate`.
        // One sampling-config snapshot drives every arm-4 decision, so the provider resolution and the gate can't disagree mid-model-switch
        let (failed_model_id, failed_base_url) = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| (c.model, c.base_url))
            .unwrap_or_default();

        // Provider-backed models recover via arm 4c below
        // The provider is resolved before the eligibility check so its warnings stay quiet for a 401 that 4c handles
        let auth_provider =
            if matches!(error.kind, SamplingErrorKind::Auth) || error.status_code == Some(401) {
                self.model_auth_provider(&failed_model_id)
            } else {
                None
            };

        let auth_recovery_eligible = matches!(error.kind, SamplingErrorKind::Auth) && {
            let gate = self.auth_gate(&failed_model_id, &failed_base_url);
            let eligible = gate.active();
            // Log the Unknown-BYOK decision (eligible or not) so a session still 401ing shows whether refresh fired
            self.log_auth_gate_unknown("handle_sampling_failure", gate, &failed_base_url);
            if !eligible && auth_provider.is_none() {
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    is_session_based = gate.is_session_based,
                    model_byok = gate.model_byok.as_str(),
                    endpoint_takes_session_credential = gate.endpoint_takes_session_credential,
                    "auth recovery: sampler 401 not refreshable (api-key auth) — surfacing 401",
                );
                xai_grok_telemetry::unified_log::warn(
                    "auth recovery: sampler 401 not eligible (api-key auth)",
                    Some(self.session_info.id.0.as_ref()),
                    Some(serde_json::json!({
                        "kind": error.kind.as_str(),
                        "status_code": error.status_code,
                        "is_session_based": gate.is_session_based,
                        "model_byok": gate.model_byok.as_str(),
                        "endpoint_takes_session_credential": gate.endpoint_takes_session_credential,
                    })),
                );
            }
            eligible
        };

        // A provider-backed model is BYOK, so its gate is inactive and the session arm (4b) can't fire; provider recovery (4c) is exclusive
        // Assert it so a future gate change trips here, not double-recovers.
        debug_assert!(
            !(auth_recovery_eligible && auth_provider.is_some()),
            "a provider-backed model must not be session-recovery-eligible"
        );

        // Observability: a 401 that did NOT classify as `Auth` kind bypasses the session arm 4b; only provider-backed models recover (4c)
        // Make that decision visible; it is otherwise indistinguishable from a failed refresh in the unified log
        if !matches!(error.kind, SamplingErrorKind::Auth)
            && error.status_code == Some(401)
            && auth_provider.is_none()
        {
            xai_grok_telemetry::unified_log::warn(
                "auth recovery: sampler 401 not eligible (non-auth error kind)",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({
                    "kind": error.kind.as_str(),
                    "status_code": error.status_code,
                })),
            );
        }

        // 4b. Auth 401: one-shot refresh and retry.
        //
        // Devboxes are not special-cased ahead of this
        // They used to be, and a devbox re-mint attempted *before* the refresh authority was wrong twice over
        // It threw away a perfectly good refresh token on any 401
        // Because `try_devbox_recovery` short-circuits on whatever is in memory, it reported success with the bearer the server had just rejected
        // That bearer was resubmitted until the turn's retry budget ran out
        // `try_recover_unauthorized`'s state machine already ends in a devbox mint, in the right place: after disk adoption and the authority
        if auth_recovery_eligible && let Some(ref am) = self.auth_manager {
            if am
                .try_recover_unauthorized(crate::auth::recovery::RecoverySource::Turn)
                .await
            {
                tracing::info!(session_id = %self.session_info.id.0, "auth recovery: sampler 401, recovered, retrying");
                xai_grok_telemetry::unified_log::info(
                    "auth recovery: sampler 401, recovered, retrying",
                    Some(self.session_info.id.0.as_ref()),
                    None,
                );
                self.prepare_sampler_for_turn().await;
                return Ok(SamplerFailureRecovery::RefreshAuthAndResubmit {
                    credential: error.credential,
                    store: RecoveredStore::SessionToken,
                });
            }
            tracing::warn!(session_id = %self.session_info.id.0, "auth recovery: sampler 401, refresh failed");
            xai_grok_telemetry::unified_log::warn(
                "auth recovery: sampler 401, refresh failed",
                Some(self.session_info.id.0.as_ref()),
                None,
            );
        }

        // 4c. Auth failure or bare 401 on a provider-backed model (gateway 401s can classify under other error kinds).
        if let Some(ref provider) = auth_provider
            && self.try_provider_401_recovery(provider).await
        {
            self.prepare_sampler_for_turn().await;
            return Ok(SamplerFailureRecovery::RefreshAuthAndResubmit {
                credential: error.credential,
                store: RecoveredStore::AuthProvider,
            });
        }
        // Custom-provider (non-xAI) 401 owned by a plugin credential: neither the
        // session-token gate nor a built-in `[auth_provider.*]` applies, so ask
        // the seam to mint a fresh bearer (e.g. an OAuth refresh). On success,
        // resubmit — the next `reconstruct_full_config` re-resolves the plugin's
        // now-persisted token. Bounded by `AuthRetrySchedule::MAX_RETRIES` in the
        // turn loop, so a plugin that keeps minting a bad token cannot spin.
        let is_credential_401 =
            matches!(error.kind, SamplingErrorKind::Auth) || error.status_code == Some(401);
        if is_credential_401
            && auth_provider.is_none()
            && refresh_custom_provider_credential(
                self.build_credential_seam(),
                &failed_base_url,
                self.model_auth_facts(&failed_model_id)
                    .auth_account
                    .as_deref(),
            )
            .await
        {
            tracing::info!(
                session_id = % self.session_info.id.0, base_url = % failed_base_url,
                "auth recovery: custom-provider 401, plugin seam refreshed credential, retrying"
            );
            xai_grok_telemetry::unified_log::info(
                "auth recovery: custom-provider 401, plugin seam refreshed credential, retrying",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({ "base_url" : failed_base_url })),
            );
            self.prepare_sampler_for_turn().await;
            // The plugin minted this credential into chat-state, not into the
            // `AuthManager`, so there is nothing to wait on there — the same
            // shape as an `[auth_provider.*]` recovery.
            return Ok(SamplerFailureRecovery::RefreshAuthAndResubmit {
                credential: error.credential,
                store: RecoveredStore::AuthProvider,
            });
        }

        // 4d. Bounded resubmit, after the auth arms, before the terminal paths.
        //     Budgeted workflow children stay terminal (guards above)
        if transient_retry_eligible(&error) && transient.enabled {
            if transient.budget_remaining() {
                // Count intercepted attempts; section 5 sees only the final one.
                if matches!(error.kind, SamplingErrorKind::IdleTimeout) {
                    self.signals_handle().record_idle_timeout();
                }
                return Ok(SamplerFailureRecovery::RetryTransient {
                    kind: error.kind,
                    status_code: error.status_code,
                });
            }
            xai_grok_telemetry::unified_log::error(
                "shell.turn.transient_retry_exhausted",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({
                    "kind": error.kind.as_str(),
                    "status_code": error.status_code,
                    "step_retries_used": transient.step_attempts,
                    "prompt_retries_used": transient.prompt_attempts,
                    "episode_elapsed_ms": transient
                        .episode_start
                        .map_or(0, |s| s.elapsed().as_millis() as u64),
                    "max_retries":
                        transient_display_ceiling(transient.step_attempts, transient.prompt_attempts),
                })),
            );
        }

        // 5. Idle timeout: record it; the generic notification and the fatal error follow.
        if matches!(error.kind, SamplingErrorKind::IdleTimeout) {
            self.signals_handle().record_idle_timeout();
        }

        // 5b. Empty response: log structured context for debugging.
        //     The model completed the stream but produced no visible content
        //     This is common with reasoning-heavy models after a successful tool call
        //     Log the full context so this is diagnosable from telemetry
        if matches!(error.kind, SamplingErrorKind::EmptyResponse) {
            if let Some(ref ctx) = error.empty_response_context {
                tracing::warn!(
                    empty_response = true,
                    empty_reason = ctx.reason.as_str(),
                    had_reasoning = ctx.had_reasoning,
                    content_len = ctx.content_len,
                    tool_call_count = ctx.tool_call_count,
                    completion_tokens = ctx.completion_tokens.unwrap_or(0),
                    reasoning_tokens = ctx.reasoning_tokens.unwrap_or(0),
                    finish_reason = ctx.finish_reason_str(),
                    first_choice_seen = ctx.first_choice_seen,
                    model = %ctx.model,
                    "empty response after retries exhausted: {reason}",
                    reason = ctx.reason,
                );
                // Stamp the doomloop magnitude onto the out-of-band capture
                // `streaming_partial.json` then records reasoning-token volume even when the reasoning text itself was clipped at the byte cap
                // This runs on the turn loop before the turn-end take, so the counts are included when the capture is uploaded
                {
                    let mut cap = self.streaming_turn_capture.lock();
                    cap.reasoning_tokens = ctx.reasoning_tokens;
                    cap.completion_tokens = ctx.completion_tokens;
                    cap.finish_reason = ctx.finish_reason.clone();
                    cap.empty_reason = Some(ctx.reason.as_str().to_owned());
                }
            }
            self.signals_handle().record_error_typed("empty_response");
        }

        // 5c and 5d. Auth diagnostics for sampling failures.
        let auth_mode = self
            .auth_manager
            .as_ref()
            .and_then(|am| am.current_or_expired())
            .map(|a| a.auth_mode)
            .unwrap_or(crate::auth::AuthMode::ApiKey);
        let auth_mode_str = format!("{auth_mode:?}");
        let client_version = xai_grok_version::VERSION;

        // 5c. Legacy WebLogin auth: always show a deprecation message regardless of error type.
        if auth_mode == crate::auth::AuthMode::WebLogin {
            let msg = format!(
                "{detailed_message}\n\n\
                 You are using a deprecated authentication method (WebLogin).\n\
                 This auth method is no longer supported and will cause errors.\n\n\
                 To fix: run `grok update`, then `grok logout`, then `grok login` to re-authenticate with OAuth2.\n\n\
                 Version: {client_version}"
            );
            self.log_terminal_failure("legacy_auth", error.status_code, &msg);
            self.send_xai_notification(XaiSessionUpdate::RetryState(
                crate::extensions::notification::RetryState::Failed {
                    error_type: "legacy_auth".to_string(),
                    message: msg.clone(),
                },
            ))
            .await;
            return Err(acp::Error::internal_error().data(msg));
        }

        // 5d. Enriched error for 404 model-not-found and 401 auth errors.
        //     Includes auth mode, client version, and available models.
        let is_model_404 =
            error.status_code == Some(404) && detailed_message.contains("does not exist");
        let is_auth_401 =
            error.status_code == Some(401) || matches!(error.kind, SamplingErrorKind::Auth);

        let detailed_message = if is_model_404 || is_auth_401 {
            let current_model = self
                .chat_state_handle
                .get_sampling_config()
                .await
                .map(|c| c.model)
                .unwrap_or_else(|| "unknown".to_string());
            // Catalog key, not the routing slug (`m.model`): several providers
            // can serve the same slug, and the key is what the user actually
            // types into `/model` to pick one of them. `current_model` above
            // is already a catalog key (see `wire_permission_auto_llm_classifier`),
            // so this keeps the two comparable.
            let available: Vec<String> = self.models_manager.models().keys().cloned().collect();
            let mut msg = format!("{detailed_message}\n");
            msg.push_str(&format!("\n  Model:     {current_model}"));
            msg.push_str(&format!("\n  Auth:      {auth_mode_str}"));
            if let Some(ref provider) = auth_provider {
                msg.push_str(&format!(
                    "\n  Provider:  [auth_provider.{}] (check the provider command and the debug log)",
                    provider.name
                ));
            }
            msg.push_str(&format!("\n  Version:   {client_version}"));
            if available.is_empty() {
                msg.push_str("\n  Available: (none)");
            } else {
                msg.push_str(&format!("\n  Available: {}", available.join(", ")));
            }

            if is_model_404 && !available.iter().any(|m| m == &current_model) {
                msg.push_str(&format!(
                    "\n\n  '{}' is not in your available models.",
                    current_model
                ));
                msg.push_str("\n  Switch models with /model or start a new session.");
            }

            msg
        } else {
            detailed_message
        };

        // 6. Generic terminal error. Tag context-window overflow distinctly so the
        //    pager can collapse it into one actionable prompt. Structured size
        //    code first, message text as fallback — same order as the
        //    compaction classifiers.
        let error_type = if error
            .error_code
            .as_ref()
            .is_some_and(xai_grok_sampling_types::ApiErrorCode::is_size_overflow)
            || xai_grok_sampling_types::is_context_length_error(&error.message)
        {
            crate::extensions::notification::CONTEXT_LENGTH_ERROR_TYPE
        } else {
            error.kind.as_str()
        };
        let (error_type, detailed_message) = if error_type == "auth" {
            let remedy = self.auth_manager.as_ref().map(|am| am.auth_remedy());
            self.classify_auth_failure(remedy, detailed_message, error.status_code)
                .await
        } else {
            (error_type, detailed_message)
        };
        self.log_terminal_failure(error_type, error.status_code, &detailed_message);
        self.send_xai_notification(XaiSessionUpdate::RetryState(
            crate::extensions::notification::RetryState::Failed {
                error_type: error_type.to_string(),
                message: detailed_message.clone(),
            },
        ))
        .await;
        Err(
            acp::Error::internal_error().data(crate::sampling::error::terminal_error_data(
                detailed_message,
                error.status_code,
                error.kind,
            )),
        )
    }

    async fn wait_for_stream_drain(
        &self,
        request_id: &xai_grok_sampler::RequestId,
        stream_drained_rx: tokio::sync::oneshot::Receiver<()>,
        timeout_message: &'static str,
    ) -> StreamDrainOutcome {
        let outcome = stream_drain_outcome(stream_drained_rx).await;
        if outcome == StreamDrainOutcome::TimedOut {
            self.mark_stream_drain_timed_out(request_id);
            tracing::warn!("{timeout_message}");
        }
        outcome
    }

    /// Drive one turn through the sampler: provider/model failover on the
    /// built-in chains first, then a subagent's 429 pacing via `budget`.
    ///
    /// The two loops are one loop on purpose. A hop switches endpoint, so its
    /// 429 belongs to the endpoint that answered, and a paced wait only makes
    /// sense once the chains have run out of somewhere else to try.
    pub(crate) async fn run_turn_via_sampler(
        self: &Arc<Self>,
        mut request: ConversationRequest,
        budget: &mut RateLimitWaitBudget,
        transient: TransientRetryState,
        mid_salvage_continuation: bool,
    ) -> Result<SamplerTurnOutcome, acp::Error> {
        // Per-turn auth refresh and sampler config push
        // Mirrors `prepare_chat_completion(false)` from the legacy path
        self.prepare_sampler_for_turn().await;
        // Snapshot the active config so failover can build model-substituted
        // variants that keep the session's auth/interceptor wiring.
        let mut active_config = self.reconstruct_full_config().await;
        active_config.idle_timeout_secs = Some(self.inference_idle_timeout.as_secs());
        let mut current_model = active_config.model.clone();
        let max_attempts = self.provider_fallback_max_attempts();
        // Counts model/provider switches only; the sampler owns its own
        // transport retry budget independently.
        let mut fallback_attempt: u32 = 0;
        loop {
            let info = match self.submit_turn_request(request.clone()).await {
                Ok(outcome) => {
                    budget.record_submission_accepted();
                    return Ok(outcome);
                }
                Err(info) => info,
            };
            // Provider/model failover: the `provider_error` hook decides
            // first, then the built-in `[[model_fallbacks]]` chains. On a
            // switch, re-issue the same request against the new model.
            if fallback_attempt + 1 < max_attempts {
                fallback_attempt += 1;
                let error_class = failover_error_class(&info);
                if let Some(new_config) = self
                    .try_provider_failover(
                        error_class,
                        &active_config,
                        &current_model,
                        fallback_attempt,
                        max_attempts,
                    )
                    .await
                {
                    // The hop's target may declare a narrower
                    // `reasoning_efforts` menu than the entry the request was
                    // built for, or no effort dial at all — carrying the
                    // origin's level across unchanged would turn one provider
                    // failure into a second, different one on the new
                    // endpoint. `catalog_key` re-derives the entry these
                    // capability lookups key on from the wire slug
                    // `try_provider_failover` just resolved to; a target this
                    // shell cannot place in the catalog (the same-provider
                    // swap fallback) falls through to the raw model string,
                    // which none of the lookups below match, so the effort is
                    // dropped rather than guessed at.
                    let target_key = self
                        .models_manager
                        .catalog_key(&new_config.model)
                        .unwrap_or_else(|| new_config.model.clone());
                    request.reasoning_effort = effort_for_hop_target(
                        request.reasoning_effort,
                        self.models_manager
                            .model_supports_reasoning_effort(&target_key),
                        &self.models_manager.model_reasoning_efforts(&target_key),
                        self.models_manager
                            .model_default_reasoning_effort(&target_key),
                    );
                    current_model = new_config.model.clone();
                    active_config = new_config;
                    continue;
                }
            }
            // Nowhere left to fail over to: a subagent may still wait out a
            // 429 rather than spending the turn. `decide` reports `Disabled`
            // for a main session, which drops straight through to recovery.
            let decision = budget.decide(&info);
            if let RateLimitWaitDecision::Wait { attempt, backoff } = decision {
                self.notify_rate_limit_wait(attempt, budget, backoff).await;
                sleep(backoff).await;
                self.prepare_sampler_for_turn().await;
                continue;
            }
            self.log_rate_limit_budget_spent(decision, &info);
            return self
                .recover_from_sampling_failure(info, budget, transient, mid_salvage_continuation)
                .await;
        }
    }

    async fn submit_turn_request(
        self: &Arc<Self>,
        request: ConversationRequest,
    ) -> Result<SamplerTurnOutcome, xai_grok_sampler::SamplingErrorInfo> {
        // Install the per-request stream-drain barrier before submitting so the drainer can acknowledge the fully processed terminal event
        let request_id = xai_grok_sampler::RequestId::random();
        let stream_drained_rx = {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.turn_stream_drained
                .lock()
                .insert(request_id.clone(), Some(tx));
            rx
        };

        let request_id_str = request_id.as_str().to_string();
        let collected = {
            let gate_span = region!("turn.sampling_gate", Parent::Inherit);
            let _permit = acquire_subagent_sampling_permit(&self.sampling_gate).await;
            gate_span.close();
            let _sampling_span = region!("turn.sampling", Parent::Inherit);
            self.sampler_handle
                .submit_and_collect_with_metadata(request_id.clone(), request)
                .await
        };
        let terminal_event_queued = collected.terminal_event_queued;
        let recovery_attempts = collected.doom_loop_recovery_attempts;
        let recovery_attempt_count = u32::try_from(recovery_attempts.len()).unwrap_or(u32::MAX);
        {
            // Cancel clears ownership before resetting the tally
            // Holding the ownership lock through reconciliation makes that boundary atomic
            // Either metadata lands before cancel and is then cleared, or cancel wins and this stale result cannot rebook the next turn
            let ownership = self.turn_stream_drained.lock();
            let counted = crate::session::doom_loop_telemetry::reconcile_request_metadata(
                &mut self.doom_loop_turn_tally.lock(),
                ownership.contains_key(&request_id),
                &request_id_str,
                &collected.doom_loop_signals,
                &recovery_attempts,
            );
            for (triggers, aborted_at_chunk) in counted {
                self.signals_handle()
                    .record_doom_loop_recovery_attempt(triggers, aborted_at_chunk);
            }
        }
        match collected.result {
            Ok((response, metrics)) => {
                // Current span is the turn span (this fn is inline-awaited from process_conversation_turn, no own #[instrument])
                let span = tracing::Span::current();
                span.record("request_id", request_id_str.as_str());
                if let Some(ttft) = metrics.time_to_first_token_ms {
                    span.record("ttft_ms", ttft as i64);
                }
                if metrics.attempts > 0 {
                    span.record("attempt", i64::from(metrics.attempts));
                }
                // A rewind/cancel clears ownership and drops the waiter
                // Do not commit that pre-boundary success or its metrics onto the restored turn
                // A real timeout remains fail-open
                if terminal_event_queued {
                    let _drain_span = region!("turn.stream_drain_barrier", Parent::Inherit);
                    if self
                        .wait_for_stream_drain(
                            &request_id,
                            stream_drained_rx,
                            "stream-drain barrier timed out; proceeding to emit tool calls",
                        )
                        .await
                        == StreamDrainOutcome::Revoked
                    {
                        return Err(revoked_sampling_info());
                    }
                } else if !self.turn_stream_drained.lock().contains_key(&request_id) {
                    return Err(revoked_sampling_info());
                }

                // The awaited result is authoritative for successful-request accounting once the request survives the turn boundary
                self.record_api_request_time();
                self.signals_handle()
                    .record_inference_metrics(metrics.clone());
                let confident_triggers = self
                    .doom_loop_recovery
                    .map(|policy| policy.confident_triggers(&response.doom_loop_signals))
                    .unwrap_or_default();
                if recovery_attempt_count > 0 && !confident_triggers.is_empty() {
                    let should_record = {
                        // Take the ownership lock, then the tally lock, the same order cancel uses to clear ownership and reset the tally
                        let ownership = self.turn_stream_drained.lock();
                        if ownership.contains_key(&request_id) {
                            let mut tally = self.doom_loop_turn_tally.lock();
                            let inserted = tally.mark_accepted_request(&request_id_str);
                            if inserted {
                                tally.merge_recovery_triggers(&confident_triggers);
                            }
                            inserted
                        } else {
                            false
                        }
                    };
                    if should_record {
                        self.signals_handle()
                            .record_doom_loop_accepted_after_budget(confident_triggers);
                    }
                }
                // A queued terminal event already released ownership when it was acknowledged above
                // Without one, release ownership after authoritative metadata accounting
                if !terminal_event_queued {
                    self.turn_stream_drained.lock().remove(&request_id);
                }
                Ok(SamplerTurnOutcome::Response(
                    Box::new(response),
                    Box::new(metrics),
                ))
            }
            Err(rich_err) => {
                // Detector labels are already merged from the awaited result.
                // Wait briefly for the UI/error event rail, then fail open so a stuck drainer cannot prevent recovery or turn teardown
                let original = xai_grok_sampler::SamplingErrorInfo::from(&rich_err);
                let outcome = if terminal_event_queued {
                    self.wait_for_stream_drain(
                        &request_id,
                        stream_drained_rx,
                        "failed-event drain barrier timed out; continuing recovery",
                    )
                    .await
                } else if self
                    .turn_stream_drained
                    .lock()
                    .remove(&request_id)
                    .is_some()
                {
                    StreamDrainOutcome::Acknowledged
                } else {
                    StreamDrainOutcome::Revoked
                };
                Err(error_after_stream_drain(outcome, original))
            }
        }
    }

    async fn recover_from_sampling_failure(
        self: &Arc<Self>,
        info: xai_grok_sampler::SamplingErrorInfo,
        budget: &RateLimitWaitBudget,
        transient: TransientRetryState,
        mid_salvage_continuation: bool,
    ) -> Result<SamplerTurnOutcome, acp::Error> {
        // Single funnel for every sampler-call failure.
        super::turn::record_failed_sample_on_turn_span(&tracing::Span::current(), info.kind);
        match self
            .handle_sampling_failure(
                info,
                budget.attempts_used(),
                transient,
                mid_salvage_continuation,
            )
            .await?
        {
            SamplerFailureRecovery::CompactAndResubmit => {
                Ok(SamplerTurnOutcome::CompactAndResubmit)
            }
            SamplerFailureRecovery::ResubmitWithoutReasoning => {
                Ok(SamplerTurnOutcome::ResubmitWithoutReasoning)
            }
            SamplerFailureRecovery::RefreshAuthAndResubmit { credential, store } => {
                Ok(SamplerTurnOutcome::RefreshAuthAndResubmit { credential, store })
            }
            SamplerFailureRecovery::RetryTransient { kind, status_code } => {
                Ok(SamplerTurnOutcome::RetryTransient { kind, status_code })
            }
        }
    }

    /// Mirror the auth-retry path's `RetryState::Retrying` marker so the paced wait is observable to the client.
    async fn notify_rate_limit_wait(
        &self,
        attempt: u32,
        budget: &RateLimitWaitBudget,
        backoff: Duration,
    ) {
        // Debug, not warn: a burst of subagents would otherwise flood the log.
        tracing::debug!(
            attempt,
            delay_ms = backoff.as_millis() as u64,
            "subagent turn rate limited; waiting for sampling capacity"
        );
        // Per-wait unified-log marker so each pause is visible in session logs like auth backoff
        // The terminal give-up alone (the `subagent_rate_limit_exhausted` marker) is not enough
        xai_grok_telemetry::unified_log::info(
            "shell.turn.subagent_rate_limit_backoff",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "attempt": attempt,
                "max_attempts": budget.max_attempts(),
                "delay_ms": backoff.as_millis() as u64,
            })),
        );
        // Whole seconds with a one-second floor: a sub-second jittered wait would otherwise render as "waiting 0s"
        let announced = Duration::from_secs(backoff.as_secs_f64().round().max(1.0) as u64);
        self.send_xai_notification(XaiSessionUpdate::RetryState(
            crate::extensions::notification::RetryState::Retrying {
                attempt,
                max_retries: budget.max_attempts(),
                reason: format!(
                    "Too many requests in flight; waiting {} before trying again",
                    human_duration(announced)
                ),
                error_type: None,
            },
        ))
        .await;
    }

    fn log_rate_limit_budget_spent(
        &self,
        decision: RateLimitWaitDecision,
        error: &xai_grok_sampler::SamplingErrorInfo,
    ) {
        let RateLimitWaitDecision::BudgetSpent { attempts, limit } = decision else {
            return;
        };
        tracing::warn!(
            attempts,
            cause = limit.as_str(),
            retry_after_secs = ?error.retry_after_secs,
            "subagent stopped waiting out rate limits; failing the turn"
        );
        xai_grok_telemetry::unified_log::warn(
            "shell.turn.subagent_rate_limit_exhausted",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "attempts": attempts,
                "cause": limit.as_str(),
                "retry_after_secs": error.retry_after_secs,
                "status_code": error.status_code,
            })),
        );
    }

    /// Proactively refresh the auth token if near expiry.
    ///
    /// Session-token path is best-effort: on success, update credentials and return.
    /// On failure, do **not** fall through to the JWT/config.toml branch when the session gate was active; that path is for BYOK JWTs only.
    /// Falling through after a failed session refresh left hard-expired opaque tokens (External/OIDC) on the wire and guaranteed a 401.
    /// Soft failures with a still-usable access token still return here (grace / optimistic send); 401 recovery remains the safety net.
    pub(crate) async fn refresh_token_if_expired(&self) {
        if let Some(ref am) = self.auth_manager {
            let creds = self.chat_state_handle.get_credentials().await;
            // Gate on the stable classifier, not `creds.auth_type`; this also heals a transient `ApiKey` flip by writing the refreshed token into `creds.api_key` below
            // See `crate::agent::auth_method::session_token_auth_gate`
            let (model_id, base_url) = self
                .chat_state_handle
                .get_sampling_config()
                .await
                .map(|c| (c.model, c.base_url))
                .unwrap_or_default();
            // Same condition as the seed guard above, so without this the re-seed puts the key back.
            if self.auth_gate(&model_id, &base_url).active()
                && ActiveAuthBackend::default().is_xai_authority()
            {
                match am.get_valid_token().await {
                    Ok(key) => {
                        if creds.api_key.as_deref() != Some(&key) {
                            let mut creds = creds;
                            creds.api_key = Some(key);
                            self.chat_state_handle.update_credentials(creds);
                        }
                        // Re-enable auth-suppressed compact; waiting for a 200 deadlocks over-window
                        self.clear_auth_compact_suppression();
                        return;
                    }
                    Err(e) => {
                        // Session path applied and failed
                        // Never fall through to JWT/config.toml reload (that branch is BYOK-only)
                        // Hard-expired: strip the chat-state seed so default headers cannot carry a dead AT (resolver is wire-valid only)
                        let hard_expired = !am.has_usable_token();
                        if hard_expired && creds.api_key.is_some() {
                            let mut cleared = creds;
                            cleared.api_key = None;
                            self.chat_state_handle.update_credentials(cleared);
                        }
                        tracing::warn!(
                            error = %e,
                            hard_expired,
                            model = %model_id,
                            "auth: preflight get_valid_token failed"
                        );
                        xai_grok_telemetry::unified_log::warn(
                            "auth.preflight.refresh_failed",
                            Some(self.session_info.id.0.as_ref()),
                            Some(serde_json::json!({
                                "error": format!("{e}"),
                                "hard_expired": hard_expired,
                                "model": model_id,
                            })),
                        );
                        return;
                    }
                }
            }
        } else {
            xai_grok_telemetry::unified_log::debug(
                "token refresh skipped: no auth manager",
                Some(self.session_info.id.0.as_ref()),
                None,
            );
        }

        // JWT from config.toml: a separate mechanism for BYOK tokens
        use crate::auth::{is_jwt_expired_or_near, parse_jwt_expiration};

        const REFRESH_THRESHOLD: chrono::Duration = chrono::Duration::minutes(5);

        let creds = self.chat_state_handle.get_credentials().await;
        let current_key = creds.api_key;
        let current_model_id = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.model)
            .unwrap_or_default();

        // Provider-backed models mint and refresh here so `resolve_credentials` stays cache-only
        if let Some(provider) = self.model_auth_provider(&current_model_id) {
            self.refresh_provider_token_pre_turn(
                &provider,
                current_key.as_deref(),
                &current_model_id,
            )
            .await;
            // Provider models carry no session JWT, so skip the JWT refresh below.
            return;
        }

        let Some(ref key) = current_key else { return };

        if !is_jwt_expired_or_near(key, REFRESH_THRESHOLD) {
            if let Some(exp) = parse_jwt_expiration(key) {
                let remaining_secs = (exp - chrono::Utc::now()).num_seconds();
                tracing::debug!(
                    model = %current_model_id,
                    remaining_secs,
                    "JWT token valid, no refresh needed"
                );
            } else {
                tracing::debug!(
                    model = %current_model_id,
                    key_len = key.len(),
                    "Token is not a JWT, expiry-based refresh not applicable"
                );
            }
            return;
        }

        // is_jwt_expired_or_near(true) guarantees parse_jwt_expiration is Some
        let remaining_secs =
            parse_jwt_expiration(key).map_or(0, |exp| (exp - chrono::Utc::now()).num_seconds());
        tracing::info!(
            model = %current_model_id,
            remaining_secs,
            "JWT near expiry, refreshing from config.toml"
        );

        let Some(new_key) = self.reload_api_key_from_config(&current_model_id) else {
            return; // specific failure already logged
        };

        if key == &new_key {
            tracing::warn!(
                model = %current_model_id,
                "Config.toml returned same token (not yet rotated by external process?)"
            );
            return;
        }

        let new_remaining_secs = parse_jwt_expiration(&new_key)
            .map_or(0, |exp| (exp - chrono::Utc::now()).num_seconds());
        tracing::info!(
            model = %current_model_id,
            new_remaining_secs,
            key_len = new_key.len(),
            "Refreshed API token from config.toml"
        );

        let mut creds = self.chat_state_handle.get_credentials().await;
        creds.api_key = Some(new_key);
        self.chat_state_handle.update_credentials(creds);
    }

    fn reload_api_key_from_config(&self, current_model_id: &str) -> Option<String> {
        let raw_config = crate::config::load_effective_config()
            .map_err(|e| tracing::warn!(error = %e, "Failed to reload config"))
            .ok()?;

        let config = crate::agent::config::Config::new_from_toml_cfg(&raw_config)
            .map_err(|e| tracing::warn!(error = %e, "Failed to parse reloaded config.toml"))
            .ok()?;

        let config_model = config
            .config_models
            .iter()
            .find(|(k, v)| v.model.as_deref().unwrap_or(k.as_str()) == current_model_id)
            .map(|(_, v)| v);

        let Some(model) = config_model else {
            tracing::warn!(
                model = %current_model_id,
                available = ?config.config_models.keys().collect::<Vec<_>>(),
                "Model not found in config.toml [model.*]"
            );
            return None;
        };

        let key = crate::agent::config::first_own_credential(
            model.api_key.as_deref(),
            model.env_key.as_ref(),
        );

        if key.is_none() {
            tracing::warn!(
                model = %current_model_id,
                env_key = ?model.env_key,
                "No api_key or env_key resolved for model"
            );
        }

        key
    }

    /// Propagate the model-reported token usage from a turn response into chat state, the per-prompt usage ledger, and per-turn signals.
    ///
    /// This is the only place per-turn `total_tokens` is refreshed in the post-sampler-refactor path.
    /// Without it `state.total_tokens` would stay frozen at the `estimate_conversation_tokens` seed from `ChatState::new`.
    /// That freezes `/context` and corrupts the resume restore that reads `meta.totalTokens` from `updates.jsonl`.
    /// Resetting `estimated_tokens_since_model = 0` here also keeps the preflight-overflow guard accurate against the next turn's tool-result deltas.
    pub(crate) fn record_response_token_usage(
        &self,
        response: &ConversationResponse,
        api_duration_ms: Option<u64>,
    ) {
        if let Some(ref u) = response.usage {
            self.tool_context
                .record_task_model_output(u64::from(u.completion_tokens));
            self.chat_state_handle
                .record_token_usage(u64::from(u.total_tokens));
            self.chat_state_handle.record_last_turn_usage(u.clone());
            self.chat_state_handle.record_model_call_usage(
                response.assistant().and_then(|a| a.model_id.clone()),
                u.clone(),
                api_duration_ms,
                response.cost_usd_ticks,
            );
            self.signals_handle()
                .record_token_usage(u.completion_tokens, u.reasoning_tokens);
        } else if self.tool_context.task_output_token_budget.is_some() {
            self.tool_context.fail_task_output_usage_closed();
            self.chat_state_handle
                .mark_usage_incomplete_nowait(true, true);
        } else if self.tool_context.sampler_retry_only_before_output {
            self.chat_state_handle
                .mark_usage_incomplete_nowait(true, true);
        }
        // TODO: a `None` usage outside these contexts is left unmarked, so a genuine mid-turn omission understates spend with no incomplete flag
    }

    /// Persist one response's items without re-estimating model output when provider usage already includes it.
    pub(super) async fn record_response_items(
        &self,
        items: Vec<ConversationItem>,
        usage_reported: bool,
    ) {
        for item in items {
            match item {
                ConversationItem::Assistant(_) => {
                    self.record_assistant_response(item, usage_reported).await;
                }
                _ if usage_reported => self.chat_state_handle.push_model_output(item),
                _ => self.chat_state_handle.push_tool_result(item),
            }
        }
    }

    pub(super) async fn record_assistant_response(
        &self,
        assistant_item: ConversationItem,
        usage_reported: bool,
    ) {
        self.signals_handle().record_assistant_message();

        // DEBUG: Log the model_id on the assistant item being recorded
        if let ConversationItem::Assistant(ref a) = assistant_item {
            tracing::info!(model_id = ?a.model_id, "DEBUG record_assistant_response model_id");
        }

        if let ConversationItem::Assistant(ref a) = assistant_item
            && let Some(first_call) = a.tool_calls.first()
        {
            tracing::info!("Assistant requested tool call: {}", first_call.id);
        }

        if usage_reported {
            self.chat_state_handle
                .push_assistant_response(assistant_item);
        } else {
            self.chat_state_handle
                .push_unreported_model_output(assistant_item);
        }
    }
}

#[cfg(test)]
mod custom_provider_auth_tests {
    use super::*;
    use crate::auth::credential_seam::{PluginCredential, PluginCredentialSeam};

    /// Resolves the credential it was built with, recording the `auth_account`
    /// selector the caller asked for.
    #[derive(Debug)]
    struct MockSeam(Option<PluginCredential>, std::sync::Mutex<Option<String>>);
    impl MockSeam {
        fn new(cred: Option<PluginCredential>) -> Self {
            Self(cred, std::sync::Mutex::new(None))
        }
        fn seen_account(&self) -> Option<String> {
            self.1.lock().unwrap().clone()
        }
    }
    #[async_trait::async_trait]
    impl PluginCredentialSeam for MockSeam {
        async fn resolve(
            &self,
            _reason: &str,
            _base_url: &str,
            account: Option<&str>,
        ) -> Option<PluginCredential> {
            *self.1.lock().unwrap() = account.map(str::to_string);
            self.0.clone()
        }
        async fn refresh(
            &self,
            _r: &str,
            _o: Option<&str>,
            _base_url: &str,
            _account: Option<&str>,
        ) -> Option<PluginCredential> {
            None
        }
        async fn start_oauth_flow(
            &self,
            _r: &str,
            _target_plugin: Option<&str>,
            _account: Option<&str>,
        ) -> Option<PluginCredential> {
            None
        }
    }

    fn oauth_cred() -> PluginCredential {
        PluginCredential {
            token: "oauth-bearer".into(),
            needs_token_auth_header: false,
            expires_at_ms: None,
            owner_id: None,
        }
    }

    /// A custom (non-xAI) provider with a subscribed plugin: the resolved OAuth
    /// bearer becomes the live bearer and auth flips to `Authorization: Bearer`
    /// regardless of the provider's static (x-api-key) scheme. This is the
    /// anthropic-OAuth acceptance at the config layer.
    #[tokio::test]
    async fn custom_provider_seam_hit_forces_bearer() {
        let seam: Arc<dyn PluginCredentialSeam> = Arc::new(MockSeam::new(Some(oauth_cred())));
        let (resolver, scheme) =
            resolve_custom_provider_auth(Some(seam), "https://api.anthropic.com", None).await;
        assert_eq!(
            resolver
                .expect("bearer resolver wired")
                .current_bearer()
                .as_deref(),
            Some("oauth-bearer")
        );
        assert_eq!(scheme, Some(xai_grok_sampler::AuthScheme::Bearer));
    }

    /// Plugin passthrough (declines to resolve): keep the provider's static key.
    #[tokio::test]
    async fn custom_provider_seam_passthrough_keeps_static_key() {
        let seam: Arc<dyn PluginCredentialSeam> = Arc::new(MockSeam::new(None));
        let (resolver, scheme) =
            resolve_custom_provider_auth(Some(seam), "https://api.anthropic.com", None).await;
        assert!(resolver.is_none());
        assert!(scheme.is_none());
    }

    /// No seam (no plugin host): unchanged static-key path.
    #[tokio::test]
    async fn custom_provider_no_seam_is_noop() {
        let (resolver, scheme) =
            resolve_custom_provider_auth(None, "https://api.anthropic.com", None).await;
        assert!(resolver.is_none());
        assert!(scheme.is_none());
    }

    /// An xAI endpoint is never routed through the plugin seam even if a plugin
    /// would resolve a credential — the first-party auth path is untouched.
    #[tokio::test]
    async fn xai_endpoint_never_uses_seam() {
        let seam: Arc<dyn PluginCredentialSeam> = Arc::new(MockSeam::new(Some(oauth_cred())));
        let (resolver, scheme) =
            resolve_custom_provider_auth(Some(seam), "https://api.x.ai/v1", None).await;
        assert!(
            resolver.is_none(),
            "xAI endpoint must not take a plugin bearer"
        );
        assert!(scheme.is_none());
    }

    /// An expired plugin credential is ignored (falls back to the static key).
    #[tokio::test]
    async fn expired_credential_falls_back() {
        let seam: Arc<dyn PluginCredentialSeam> = Arc::new(MockSeam::new(Some(PluginCredential {
            token: "stale".into(),
            needs_token_auth_header: false,
            expires_at_ms: Some(1),
            owner_id: None,
        })));
        let (resolver, scheme) =
            resolve_custom_provider_auth(Some(seam), "https://api.anthropic.com", None).await;
        assert!(resolver.is_none());
        assert!(scheme.is_none());
    }

    /// The model's configured `auth_account` reaches the plugin, so one sidecar
    /// can hold several accounts for the same custom endpoint.
    #[tokio::test]
    async fn custom_provider_forwards_configured_account() {
        let seam = Arc::new(MockSeam::new(Some(oauth_cred())));
        let (resolver, _) = resolve_custom_provider_auth(
            Some(seam.clone() as Arc<dyn PluginCredentialSeam>),
            "https://example.test/v1",
            Some("work"),
        )
        .await;
        assert!(resolver.is_some());
        assert_eq!(seam.seen_account().as_deref(), Some("work"));
    }

    /// No configured `auth_account` → nothing extra reaches the plugin, which is
    /// exactly the pre-selector behaviour.
    #[tokio::test]
    async fn custom_provider_without_account_sends_none() {
        let seam = Arc::new(MockSeam::new(Some(oauth_cred())));
        let (resolver, _) = resolve_custom_provider_auth(
            Some(seam.clone() as Arc<dyn PluginCredentialSeam>),
            "https://example.test/v1",
            None,
        )
        .await;
        assert!(resolver.is_some());
        assert!(seam.seen_account().is_none());
    }

    /// A seam whose `refresh` mints a fresh credential (the resolve arm is
    /// irrelevant to the 401-recovery path).
    #[derive(Debug)]
    struct MockRefreshSeam(Option<PluginCredential>, std::sync::Mutex<Option<String>>);
    impl MockRefreshSeam {
        fn new(cred: Option<PluginCredential>) -> Self {
            Self(cred, std::sync::Mutex::new(None))
        }
        fn seen_account(&self) -> Option<String> {
            self.1.lock().unwrap().clone()
        }
    }
    #[async_trait::async_trait]
    impl PluginCredentialSeam for MockRefreshSeam {
        async fn resolve(
            &self,
            _reason: &str,
            _base_url: &str,
            _account: Option<&str>,
        ) -> Option<PluginCredential> {
            None
        }
        async fn refresh(
            &self,
            _r: &str,
            _o: Option<&str>,
            _base_url: &str,
            account: Option<&str>,
        ) -> Option<PluginCredential> {
            *self.1.lock().unwrap() = account.map(str::to_string);
            self.0.clone()
        }
        async fn start_oauth_flow(
            &self,
            _r: &str,
            _target_plugin: Option<&str>,
            _account: Option<&str>,
        ) -> Option<PluginCredential> {
            None
        }
    }

    /// Custom (non-xAI) provider 401: the seam refreshes → recovery fires.
    #[tokio::test]
    async fn custom_provider_refresh_hit_recovers() {
        let seam: Arc<dyn PluginCredentialSeam> =
            Arc::new(MockRefreshSeam::new(Some(oauth_cred())));
        assert!(
            refresh_custom_provider_credential(Some(seam), "https://api.anthropic.com", None).await
        );
    }

    /// The plugin declines to refresh (passthrough): no recovery, the 401
    /// surfaces as before.
    #[tokio::test]
    async fn custom_provider_refresh_passthrough_no_recovery() {
        let seam: Arc<dyn PluginCredentialSeam> = Arc::new(MockRefreshSeam::new(None));
        assert!(
            !refresh_custom_provider_credential(Some(seam), "https://api.anthropic.com", None)
                .await
        );
    }

    /// No seam (no plugin host): the 401 is not seam-recoverable.
    #[tokio::test]
    async fn custom_provider_refresh_no_seam_no_recovery() {
        assert!(!refresh_custom_provider_credential(None, "https://api.anthropic.com", None).await);
    }

    /// The failed model's `auth_account` rides the 401 refresh, so the plugin
    /// re-mints the account the model is configured for rather than guessing
    /// from `base_url` alone (two accounts can share one endpoint).
    #[tokio::test]
    async fn custom_provider_refresh_forwards_configured_account() {
        let seam = Arc::new(MockRefreshSeam::new(Some(oauth_cred())));
        assert!(
            refresh_custom_provider_credential(
                Some(seam.clone() as Arc<dyn PluginCredentialSeam>),
                "https://example.test/v1",
                Some("personal"),
            )
            .await
        );
        assert_eq!(seam.seen_account().as_deref(), Some("personal"));
    }

    /// An xAI endpoint is never refreshed through the plugin seam even if the
    /// plugin would mint one — first-party 401s heal via the `AuthManager` path.
    #[tokio::test]
    async fn xai_endpoint_never_refreshes_via_seam() {
        let seam: Arc<dyn PluginCredentialSeam> =
            Arc::new(MockRefreshSeam::new(Some(oauth_cred())));
        assert!(
            !refresh_custom_provider_credential(Some(seam), "https://api.x.ai/v1", None).await,
            "xAI endpoint must not take a plugin refresh"
        );
    }
}
/// Per-tool precedence: a non-empty `over` wins, else the non-empty `seed`.
fn prefer_non_empty<T>(
    over: Option<T>,
    seed: Option<T>,
    is_empty: impl Fn(&T) -> bool,
) -> Option<T> {
    over.filter(|o| !is_empty(o))
        .or_else(|| seed.filter(|s| !is_empty(s)))
}

/// Acquire the turn-sampling permit for this session, or `None` when the gate is `None` (ungated).
/// A subagent's excess turns queue on `acquire_owned`; the permit releases on drop.
/// The semaphore is never closed, so `.ok()` fails open to ungated for this turn.
async fn acquire_subagent_sampling_permit(
    gate: &Option<Arc<tokio::sync::Semaphore>>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let semaphore = gate.as_ref()?;
    semaphore.clone().acquire_owned().await.ok()
}

/// The cutoff a subagent inherits: a non-empty per-turn `base` wins per tool, else the `seed`.
fn resolve_configured_cutoff(
    seed: Option<xai_grok_sampling_types::ToolOverrides>,
    base: Option<&xai_grok_sampling_types::ToolOverrides>,
) -> xai_grok_sampling_types::ToolOverrides {
    use xai_grok_sampling_types::{ToolOverrides, WebSearchOptions, XSearchOptions};
    let ToolOverrides {
        x_search: seed_x,
        web_search: seed_w,
    } = seed.unwrap_or_default();
    let (over_x, over_w) =
        base.map_or((None, None), |b| (b.x_search.clone(), b.web_search.clone()));
    ToolOverrides {
        x_search: prefer_non_empty(over_x, seed_x, XSearchOptions::is_empty),
        web_search: prefer_non_empty(over_w, seed_w, WebSearchOptions::is_empty),
    }
}

/// The Auto-mode permission classifier is only as good as the schema that
/// actually reaches the provider. These assert on the **emitted request body**
/// for each wire format, because a schema held in `ConversationRequest` and
/// dropped by the format builder is exactly the failure that made every
/// permission decision fall back to a heuristic without a visible error.
#[cfg(test)]
mod permission_classifier_request_tests {
    use xai_grok_sampling_types::{ApiBackend, ReasoningEffort};
    use xai_grok_workspace::permission::{
        ClassifierMessage, ClassifierMessageRole, classifier_output_json_schema,
    };

    fn messages() -> Vec<ClassifierMessage> {
        vec![
            ClassifierMessage {
                role: ClassifierMessageRole::System,
                text: "you review a command".to_owned(),
            },
            ClassifierMessage {
                role: ClassifierMessageRole::User,
                text: "## Proposed action\ntool: run_terminal_command".to_owned(),
            },
        ]
    }

    fn request(backend: &ApiBackend) -> xai_grok_sampling_types::ConversationRequest {
        super::build_permission_classifier_request(
            backend,
            "some-model".to_owned(),
            messages(),
            Some(ReasoningEffort::Low),
            "session-1".to_owned(),
        )
    }

    /// Messages has no usable native response schema (`output_config.format`
    /// needs an `anthropic-beta` opt-in this client never sends), so the verdict
    /// schema must ride as the `StructuredOutput` tool's `input_schema` — and
    /// must be there in the serialized body, not merely on the request struct.
    #[test]
    fn messages_classifier_request_carries_the_schema_as_a_tool() {
        let req = request(&ApiBackend::Messages);
        let wire =
            serde_json::to_value(xai_grok_sampling_types::build_messages_request(&req, None))
                .unwrap();

        let tools = wire["tools"].as_array().expect("tools must be emitted");
        assert_eq!(tools.len(), 1, "{wire:#}");
        assert_eq!(tools[0]["name"], "StructuredOutput", "{wire:#}");
        assert_eq!(
            tools[0]["input_schema"],
            classifier_output_json_schema(),
            "the classifier schema must reach the wire verbatim: {wire:#}",
        );
        // Nothing may depend on the beta-gated field: a schema parked there is
        // not enforced, which is precisely how this went unnoticed.
        assert!(
            wire.pointer("/output_config/format").is_none(),
            "output_config.format is beta-gated and must not be relied on: {wire:#}",
        );
        // A forced tool_choice is rejected alongside extended thinking, which
        // the classifier turns on whenever a reasoning effort resolves.
        assert!(wire.get("tool_choice").is_none(), "{wire:#}");
        assert!(
            wire.get("thinking").is_some(),
            "reasoning effort must still map to thinking: {wire:#}",
        );
    }

    /// Gemini enforces `generationConfig.responseJsonSchema` natively, so the
    /// classifier stays on the native path there and sends no tool. The schema
    /// must land verbatim — `additionalProperties` included, which is why
    /// `responseJsonSchema` and not the OpenAPI-3.0-subset `responseSchema` is
    /// the field it maps onto.
    #[test]
    fn gemini_classifier_request_carries_the_schema_natively() {
        let req = request(&ApiBackend::Gemini);
        assert!(req.tools.is_empty(), "gemini needs no synthetic tool");
        let wire =
            serde_json::to_value(xai_grok_sampling_types::build_gemini_request(&req)).unwrap();

        assert_eq!(
            wire.pointer("/generationConfig/responseJsonSchema"),
            Some(&classifier_output_json_schema()),
            "the classifier schema must reach the wire verbatim: {wire:#}",
        );
        assert_eq!(
            wire.pointer("/generationConfig/responseMimeType")
                .and_then(serde_json::Value::as_str),
            Some("application/json"),
            "a schema without the json mime type is rejected: {wire:#}",
        );
        assert_eq!(
            wire.pointer("/generationConfig/responseJsonSchema/additionalProperties"),
            Some(&serde_json::Value::Bool(false)),
            "the strict shape must survive; `responseSchema` could not hold it: {wire:#}",
        );
        assert!(wire.get("tools").is_none(), "{wire:#}");
    }

    /// The two natively-constrained OpenAI-shaped formats keep the schema on
    /// `json_schema` and gain no synthetic tool.
    #[test]
    fn openai_shaped_classifier_requests_stay_on_the_native_schema() {
        for backend in [ApiBackend::ChatCompletions, ApiBackend::Responses] {
            let req = request(&backend);
            assert_eq!(
                req.json_schema.as_ref(),
                Some(&classifier_output_json_schema()),
                "{backend:?}",
            );
            assert!(req.tools.is_empty(), "{backend:?}");
        }
    }

    /// On the tool path the verdict arrives as tool-call arguments, so reading
    /// assistant text would find nothing; on the native path it is the text.
    #[test]
    fn verdict_text_prefers_the_structured_output_tool_call() {
        use xai_grok_sampling_types::{
            AssistantItem, ConversationItem, ConversationResponse, ToolCall,
        };
        let response = |items: Vec<ConversationItem>| ConversationResponse {
            items,
            stop_reason: None,
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        let verdict = r#"{"thinking":"t","shouldBlock":false,"reason":"r"}"#;
        let with_tool_call = response(vec![ConversationItem::Assistant(AssistantItem {
            content: "".into(),
            tool_calls: vec![ToolCall {
                id: "id-1".into(),
                name: "StructuredOutput".to_owned(),
                arguments: verdict.into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        })]);
        assert_eq!(super::classifier_verdict_text(&with_tool_call), verdict);

        let text_only = response(vec![ConversationItem::assistant(verdict.to_owned())]);
        assert_eq!(super::classifier_verdict_text(&text_only), verdict);
    }
}
#[path = "sampler_turn_tests.rs"]
mod tests;

#[cfg(test)]
mod stream_drain_tests {
    use super::{StreamDrainOutcome, error_after_stream_drain, stream_drain_outcome};

    #[tokio::test(start_paused = true)]
    async fn acknowledged_barrier_is_distinct_from_revocation() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(()).expect("receiver stays live");
        assert_eq!(
            stream_drain_outcome(rx).await,
            StreamDrainOutcome::Acknowledged
        );

        let (tx, rx) = tokio::sync::oneshot::channel();
        drop(tx);
        assert_eq!(stream_drain_outcome(rx).await, StreamDrainOutcome::Revoked);
    }

    #[tokio::test(start_paused = true)]
    async fn live_stalled_barrier_times_out() {
        let (_tx, rx) = tokio::sync::oneshot::channel();
        assert_eq!(stream_drain_outcome(rx).await, StreamDrainOutcome::TimedOut);
    }

    #[test]
    fn failed_drain_revocation_supersedes_original_error() {
        let original = xai_grok_sampler::SamplingErrorInfo {
            kind: xai_grok_sampler::SamplingErrorKind::RateLimited,
            status_code: Some(429),
            message: "original sampling failure".to_string(),
            is_retryable: true,
            retry_after_secs: Some(1),
            should_retry: None,
            error_code: None,
            model_metadata: None,
            empty_response_context: None,
            doom_loop_triggers: None,
            doom_loop_aborted_at_chunk: None,
            credential: xai_grok_sampling_types::SentCredential::Unknown,
        };
        let result = error_after_stream_drain(StreamDrainOutcome::Revoked, original);
        assert_eq!(
            result.message,
            "sampling result revoked by turn cancellation or rewind"
        );
        assert!(!result.is_retryable);
    }
}
