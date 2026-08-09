//! HTTP client for the xAI sampling APIs.
//!
//! Owns the `reqwest::Client`, default request headers, and per-method
//! defaults. Talks to three backend shapes:
//!
//! * Chat Completions (`/chat/completions`)
//! * Responses API (`/responses`)
//! * Anthropic Messages API (`/messages`)
//!
//! All trace-upload and URL-based header injection is intentionally
//! *not* here. The session is responsible for putting any per-request
//! headers (proxy auth, OTel context, etc.)
//! into [`SamplerConfig::extra_headers`] before constructing the client.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use indexmap::IndexMap;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT,
};
use serde::Serialize;

use xai_grok_sampling_types::error::{try_parse_stream_error, user_facing_api_error_message};
use xai_grok_sampling_types::gemini::{GeminiRequest, GeminiStreamChunk};
use xai_grok_sampling_types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ConversationRequest,
    ConversationResponse, CreateResponseWrapper, DOOM_LOOP_CHECK_HEADER, MessagesRequestWrapper,
    ResponseModelMetadata, Result, SamplingError, SentCredential, build_gemini_request,
    build_messages_request,
    is_check_event, messages, rs,
};

use crate::concurrency::{AdmissionPermit, ConcurrencyClass, admit, hold_permit_for_stream};
use crate::config::{AuthScheme, OriginClientInfo, SamplerConfig};
use crate::events::SamplingErrorInfo;
use xai_grok_auth::bearer_suffix;

// Re-export ApiBackend from the shared types crate for downstream callers.
pub use xai_grok_sampling_types::ApiBackend;

/// Process-level fallback for the `x-grok-client-identifier` header.
const DEFAULT_CLIENT_IDENTIFIER: &str = "grok-shell";

/// Product identifier baked into User-Agent strings.
const AGENT_PRODUCT: &str = "grok-shell";

/// Diagnostic for a `messages` backend whose model config declares no output
/// ceiling.
///
/// The Messages API requires `max_tokens` on every request and rejects (400)
/// any value above the *target model's own* output limit. That limit is not
/// universal: current top-tier Claude models allow 128K output tokens, while
/// smaller ones cap lower (Haiku-class models are 64K). Nothing reachable from
/// `apply_message_defaults` identifies which model is on the other end
/// -- [`SamplerConfig::max_completion_tokens`] is the only *declared* output
/// ceiling, and [`SamplerConfig::context_window`] is an input budget, not an
/// output cap. So any built-in default is a guess, and the previous 128K guess
/// made every request against a lower-ceiling model fail as a generic provider
/// 400 that the user had to reverse-engineer. Refuse the guess instead and say
/// what to set.
const MISSING_MAX_COMPLETION_TOKENS: &str = "the messages backend requires max_completion_tokens: \
     this model's config declares no output-token ceiling, and the Messages API rejects a \
     max_tokens above the model's own limit (which the client cannot discover). Set \
     max_completion_tokens on the model (or provider) entry to the model's documented \
     max output tokens.";

/// Per-request `x-grok-*` headers. Optional fields are skipped when empty/`None`.
struct GrokRequestHeaders<'a> {
    conv_id: &'a str,
    req_id: &'a str,
    model_id: &'a str,
    session_id: &'a str,
    turn_idx: Option<&'a str>,
    agent_id: &'a str,
    deployment_id: Option<&'a str>,
    user_id: Option<&'a str>,
}

impl GrokRequestHeaders<'_> {
    fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut b = builder
            .header("x-grok-conv-id", self.conv_id)
            .header("x-grok-req-id", self.req_id)
            .header("x-grok-model-override", self.model_id)
            .header("x-grok-session-id", self.session_id)
            .header("x-grok-agent-id", self.agent_id);
        if let Some(idx) = self.turn_idx {
            b = b.header("x-grok-turn-idx", idx);
        }
        if let Some(id) = self.deployment_id.filter(|s| !s.is_empty()) {
            b = b.header("x-grok-deployment-id", id);
        }
        if let Some(id) = self.user_id.filter(|s| !s.is_empty()) {
            b = b.header("x-grok-user-id", id);
        }
        b
    }
}

/// Parse the `Retry-After` response header as delta-seconds.
/// Our inference backends only emit integer seconds (never HTTP-date),
/// so we only handle that form. HTTP-dates silently return `None` and
/// the caller falls back to exponential backoff.
/// Capped at 120s to prevent absurdly long sleeps from a misbehaving upstream.
/// Deserialize a Responses API SSE event, with a fallback for xAI-specific
/// tool types (e.g., `x_search`) that `async_openai` can't parse.
///
/// The API echoes the request's `tools` array in `ResponseCompleted` and
/// `ResponseCreated` events. If we sent `{"type": "x_search"}`, the response
/// includes it, and `rs::Tool` deserialization fails. On failure, we strip
/// unrecognized tools from the raw JSON and retry.
///
/// On `response.completed` / `response.incomplete`, this also rewrites
/// `response.usage.total_tokens` in place to the live context length
/// (`context_details.input_tokens + context_details.output_tokens`)
/// when the API emits the xAI-specific `context_details` field.
/// Async-openai's typed `ResponseUsage` doesn't model `context_details`,
/// so we peek the raw JSON for it. The cumulative `input_tokens` /
/// `output_tokens` / `cached_tokens` continue to flow from the typed
/// `ResponseUsage` unchanged so billing telemetry stays correct. When
/// the API doesn't emit `context_details` (older deployments) `total_tokens`
/// passes through unchanged.
///
/// `Ok(None)` means "skip this frame": the event did not deserialize and its
/// `type` is not one [`CONSUMED_EVENT_TYPES`] lists, so no part of the
/// pipeline would have read it. That covers vendor extensions and gateway
/// heartbeats (`keepalive`) alike — the event set is open-ended and
/// `rs::ResponseStreamEvent` is a closed enum pinned by revision, so a frame
/// nobody reads must not be able to kill a turn. A frame whose `type` *is*
/// consumed still fails with [`SamplingError::Serialization`]: that one is
/// malformed data we would otherwise lose silently.
///
/// The distinction is drawn on the peeked `type` field, never on the serde
/// error text: the error string is not a stable interface, and "unknown
/// variant" and "invalid type at field x" would be indistinguishable to any
/// caller reading it.
fn deserialize_response_event(data: &str) -> Result<Option<rs::ResponseStreamEvent>> {
    let first_err = match serde_json::from_str::<rs::ResponseStreamEvent>(data) {
        Ok(mut event) => {
            apply_terminal_event_overrides(&mut event, data);
            return Ok(Some(event));
        }
        Err(first_err) => first_err,
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data) else {
        tracing::error!(
            error = %first_err,
            raw_data = %data,
            "Failed to deserialize ResponseStreamEvent from stream"
        );
        return Err(SamplingError::Serialization(first_err));
    };
    let event_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !crate::stream::responses::CONSUMED_EVENT_TYPES.contains(&event_type) {
        log_skipped_response_event_once(event_type);
        return Ok(None);
    }
    // Strip tools that async_openai's rs::Tool can't deserialize
    // (e.g., xAI-specific "x_search"). Instead of maintaining a
    // hardcoded allowlist, try deserializing each tool entry —
    // if it fails, drop it.
    if let Some(tools) = value
        .pointer_mut("/response/tools")
        .and_then(|v| v.as_array_mut())
    {
        tools.retain(|t| serde_json::from_value::<rs::Tool>(t.clone()).is_ok());
    }
    if let Ok(mut event) = serde_json::from_value::<rs::ResponseStreamEvent>(value) {
        apply_terminal_event_overrides(&mut event, data);
        return Ok(Some(event));
    }
    tracing::error!(
        error = %first_err,
        raw_data = %data,
        "Failed to deserialize ResponseStreamEvent from stream"
    );
    Err(SamplingError::Serialization(first_err))
}

/// Event types already reported by [`log_skipped_response_event_once`].
static SKIPPED_RESPONSE_EVENT_TYPES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Report a skipped Responses API event type once per process.
///
/// Once per *type*, not per event: a gateway heartbeat arrives every few
/// seconds for the life of every stream, and logging each one would bury the
/// log while saying nothing new. The first frame of a type is the only one
/// that carries information — that the gateway emits it at all.
fn log_skipped_response_event_once(event_type: &str) {
    let first_sighting = match SKIPPED_RESPONSE_EVENT_TYPES.lock() {
        Ok(mut seen) => seen.insert(event_type.to_owned()),
        // A poisoned set only costs repeated logging; never drop the report.
        Err(_) => true,
    };
    if first_sighting {
        tracing::warn!(
            event_type = %event_type,
            "responses stream: skipping unrecognized event type"
        );
    }
}

/// On terminal Responses API events (`response.completed` /
/// `response.incomplete`), rewrite `response.usage.total_tokens` to the
/// live context length when the wire includes
/// `response.usage.context_details.{input_tokens, output_tokens}`.
///
/// `total_tokens` drives the CLI's `/context` bar, the auto-compact
/// threshold, and `meta.totalTokens` on persisted sessions. Under
/// server-side multi-turn loops (e.g. `web_search`, `x_search`) the
/// wire's cumulative total inflates as the loop runs; `context_details`
/// reports the final turn's prompt + output tokens — the real live
/// context the model is sitting in. Billing fields
/// (`input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens`,
/// `output_tokens_details.reasoning_tokens`) stay on the cumulative
/// wire values so telemetry is unaffected.
///
/// No-op when:
/// - the event is not terminal,
/// - `response.usage` is `None`,
/// - `context_details` is absent (older backends / non-loop responses),
/// - or either of `context_details.{input_tokens, output_tokens}` is
///   missing — we don't guess the missing half.
fn apply_terminal_event_overrides(event: &mut rs::ResponseStreamEvent, data: &str) {
    let response = match event {
        rs::ResponseStreamEvent::ResponseCompleted(e) => &mut e.response,
        rs::ResponseStreamEvent::ResponseIncomplete(e) => &mut e.response,
        _ => return,
    };
    // Re-parse for fields async_openai's types omit (context total, cost ticks).
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    // Stash cost ticks in metadata for stream_responses.
    if let Some(ticks) = xai_grok_sampling_types::reported_cost_ticks(
        value
            .pointer("/response/usage/cost_in_usd_ticks")
            .and_then(|v| v.as_i64()),
    ) {
        response
            .metadata
            .get_or_insert_with(Default::default)
            .insert(COST_USD_TICKS_METADATA_KEY.to_owned(), ticks.to_string());
    }
    let Some(usage) = response.usage.as_mut() else {
        return;
    };
    let Some(total) = extract_context_total(&value) else {
        return;
    };
    usage.total_tokens = total;
}

/// Metadata key for cost ticks past typed Response events.
pub(crate) const COST_USD_TICKS_METADATA_KEY: &str = "xai.cost_usd_ticks";

/// Read `response.usage.context_details.{input_tokens, output_tokens}`
/// from the parsed terminal-event JSON and return their sum. Returns `None`
/// if either field is missing or out of `u32` range.
fn extract_context_total(value: &serde_json::Value) -> Option<u32> {
    let cd = value.pointer("/response/usage/context_details")?;
    let i = u32::try_from(cd.get("input_tokens")?.as_u64()?).ok()?;
    let o = u32::try_from(cd.get("output_tokens")?.as_u64()?).ok()?;
    Some(i.saturating_add(o))
}

/// Record `success=false` + `error` on the active inference span when a stream
/// request fails before any response (transport/connect/TLS errors). Without
/// this the `#[instrument]` span closes with both fields Empty, so an outage
/// shows zero `success=false` and error-rate alerts never fire.
fn record_stream_request_failure(err: &reqwest::Error) {
    let span = tracing::Span::current();
    span.record("success", false);
    span.record("error", err.to_string().as_str());
}

fn extract_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| s.min(120))
}

fn extract_should_retry(headers: &reqwest::header::HeaderMap) -> Option<bool> {
    headers
        .get("x-should-retry")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            if s.eq_ignore_ascii_case("true") {
                Some(true)
            } else if s.eq_ignore_ascii_case("false") {
                Some(false)
            } else {
                None // unknown value — treat as absent
            }
        })
}

fn extract_model_metadata(headers: &reqwest::header::HeaderMap) -> Option<ResponseModelMetadata> {
    let context_window = headers
        .get("x-grok-context-window")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let max_completion_tokens = headers
        .get("x-grok-max-completion-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());

    let models_etag = headers
        .get("x-models-etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if context_window.is_some() || max_completion_tokens.is_some() || models_etag.is_some() {
        Some(ResponseModelMetadata {
            context_window,
            max_completion_tokens,
            models_etag,
        })
    } else {
        None
    }
}

/// Wrapper for streaming chat completion requests that adds `stream` and
/// `stream_options` fields without modifying the original `ChatCompletionRequest`.
///
/// Uses `#[serde(flatten)]` to inline all fields from the inner request,
/// allowing single-pass serialization instead of the previous two-pass
/// approach (serialize to `Value`, mutate, serialize to bytes).
#[derive(Serialize)]
struct StreamingChatRequest<'a> {
    #[serde(flatten)]
    inner: &'a ChatCompletionRequest,
    stream: bool,
    stream_options: StreamOptions,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// Resolve `env_http_headers` (`header -> env var`) into `headers` via `getenv`, skipping unset/blank/invalid entries and trimming values.
fn apply_env_http_headers(
    env_http_headers: &IndexMap<String, String>,
    getenv: impl Fn(&str) -> Option<String>,
    headers: &mut HeaderMap,
) {
    for (key, env_var) in env_http_headers {
        let Some(value) = getenv(env_var) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let (Ok(name), Ok(header_value)) = (
            HeaderName::try_from(key.as_str()),
            HeaderValue::from_str(value),
        ) else {
            tracing::warn!(
                header = %key,
                env_var = %env_var,
                "skipping env_http_header with an invalid header name or value"
            );
            continue;
        };
        headers.insert(name, header_value);
    }
}

/// HTTP client for sampling. Cheap to clone; carries an `Arc`-backed
/// `reqwest::Client` and the default headers/request-defaults computed from a
/// [`SamplerConfig`] at construction time.
#[derive(Clone)]
pub struct SamplingClient {
    http: reqwest::Client,
    default_headers: HeaderMap,
    base_url: String,
    defaults: ClientDefaults,
    /// Optional 401-attribution hook. The shell wires this to emit a
    /// structured event at every UNAUTHORIZED arm so 401s can be
    /// bucketed by stale-snapshot vs. live-token-rejected. `None` for
    /// sampler-only callers and tests.
    attribution_callback: Option<crate::attribution::SharedAttributionCallback>,
    /// Per-request bearer override. See `SamplerConfig::bearer_resolver`.
    bearer_resolver: Option<crate::config::SharedBearerResolver>,
    /// Per-request header injection (OTel traceparent).
    header_injector: Option<crate::config::SharedHeaderInjector>,
    /// Outbound request interceptor. See `SamplerConfig::request_interceptor`.
    request_interceptor: Option<crate::intercept::SharedRequestInterceptor>,
    /// Provider/stream error hook. See `SamplerConfig::error_hook`.
    error_hook: Option<crate::intercept::SharedErrorHook>,
    /// Endpoint URL builder, resolved once from `base_url` + `query_params`.
    endpoint: EndpointTemplate,
}

impl std::fmt::Debug for SamplingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamplingClient")
            .field("base_url", &self.base_url)
            .field("defaults", &self.defaults)
            .field(
                "has_attribution_callback",
                &self.attribution_callback.is_some(),
            )
            .field("has_bearer_resolver", &self.bearer_resolver.is_some())
            .field(
                "has_request_interceptor",
                &self.request_interceptor.is_some(),
            )
            .field("has_error_hook", &self.error_hook.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
struct ClientDefaults {
    model: String,
    max_completion_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    api_backend: ApiBackend,
    auth_scheme: AuthScheme,
    stream_tool_calls: bool,
    doom_loop_recovery: Option<xai_grok_sampling_types::DoomLoopRecoveryPolicy>,
    /// Declared `thinking` dialect for the messages backend; see
    /// [`crate::SamplerConfig::thinking`].
    thinking: Option<xai_grok_sampling_types::ThinkingDialect>,
    /// Declared parallelism cap for this endpoint; see
    /// [`crate::SamplerConfig::max_concurrent`].
    max_concurrent: Option<std::num::NonZeroUsize>,
    /// Which admission lane this client's requests take; see
    /// [`crate::SamplerConfig::concurrency_class`].
    concurrency_class: ConcurrencyClass,
}

/// Endpoint URL builder, resolved once at client construction so each request
/// only appends its path.
#[derive(Clone, Debug)]
enum EndpointTemplate {
    /// No query params and no query on the base URL (or an unparseable base):
    /// append the path to the base verbatim.
    Plain(String),
    /// Query params configured: `{prefix}/{path}{suffix}`. `suffix` starts with
    /// `?` and folds any base-URL params, with a configured key winning over the
    /// same key in `base_url` (percent-encoded, no duplicates).
    WithQuery { prefix: String, suffix: String },
}

impl EndpointTemplate {
    fn new(base_url: &str, query_params: &IndexMap<String, String>) -> Self {
        let base = base_url.trim_end_matches('/').to_string();
        // The fast path is safe only when there is nothing to fold: no configured
        // params and no query already on the base (which would otherwise land
        // before the appended path).
        if query_params.is_empty() && !base.contains('?') {
            return Self::Plain(base);
        }
        let mut url = match reqwest::Url::parse(&base) {
            Ok(url) => url,
            Err(error) => {
                tracing::warn!(
                    url = %base,
                    %error,
                    "failed to parse base URL for endpoint; sending without folded query"
                );
                return Self::Plain(base);
            }
        };
        let overridden: std::collections::HashSet<&str> =
            query_params.keys().map(String::as_str).collect();
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| !overridden.contains(k.as_ref()))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let prefix = {
            let mut prefix_url = url.clone();
            prefix_url.set_query(None);
            prefix_url.as_str().trim_end_matches('/').to_string()
        };
        {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            for (key, value) in &kept {
                pairs.append_pair(key, value);
            }
            for (key, value) in query_params {
                pairs.append_pair(key, value);
            }
        }
        let suffix = url.query().map(|q| format!("?{q}")).unwrap_or_default();
        Self::WithQuery { prefix, suffix }
    }

    fn url_for_path(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        match self {
            Self::Plain(base) => format!("{base}/{path}"),
            Self::WithQuery { prefix, suffix } => format!("{prefix}/{path}{suffix}"),
        }
    }
}

// =============================================================================
// User-Agent helpers
// =============================================================================

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlatformInfo {
    os: String,
    arch: String,
}

impl PlatformInfo {
    fn current() -> Self {
        let os = match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            other => other,
        }
        .to_string();

        let arch = match std::env::consts::ARCH {
            "arm64" => "aarch64",
            "x86_64" => "x86_64",
            other => other,
        }
        .to_string();

        Self { os, arch }
    }
}

fn agent_version() -> String {
    xai_grok_version::VERSION.to_string()
}

/// Render a User-Agent string for the given origin client.
///
/// Mirrors the shell's `user_agent_string_for` but uses sampler-local
/// constants. The session typically owns the canonical User-Agent
/// rendering for process-wide HTTP clients; this helper is for
/// per-session sampling clients that want to override it.
pub fn user_agent_string_for(origin: &OriginClientInfo) -> String {
    let agent_version = agent_version();
    let platform = PlatformInfo::current();

    if origin.product == AGENT_PRODUCT && origin.version.as_deref() == Some(agent_version.as_str())
    {
        return format!(
            "{}/{} ({}; {})",
            AGENT_PRODUCT, agent_version, platform.os, platform.arch
        );
    }

    match origin.version.as_deref() {
        Some(origin_version) => format!(
            "{}/{} {}/{} ({}; {})",
            origin.product,
            origin_version,
            AGENT_PRODUCT,
            agent_version,
            platform.os,
            platform.arch
        ),
        None => format!(
            "{} {}/{} ({}; {})",
            origin.product, AGENT_PRODUCT, agent_version, platform.os, platform.arch
        ),
    }
}

/// A request builder coupled to the credential state it was built with, so
/// a 401 arm cannot classify from anything but the build-time capture. The
/// wire default (`SentCredential::Unknown`, which charges the retry budget)
/// stays the fail-closed one; only an explicit `sent_bearer: None` — a send
/// the builder provably stamped no credential onto — reaches the uncharged
/// lane via [`auth_rejected`].
struct SentRequest {
    builder: reqwest::RequestBuilder,
    /// Tail fragment of the credential in the built headers (`None` = no
    /// credential header at all).
    sent_bearer: Option<String>,
}

/// The one way a 401 becomes a `SamplingError::Auth` with a wire-derived
/// credential classification: from the fragment its [`SentRequest`] captured.
fn auth_rejected(message: String, sent_bearer: Option<&str>) -> SamplingError {
    SamplingError::Auth {
        message,
        credential: SentCredential::from_sent_fragment(sent_bearer),
    }
}

// =============================================================================
// SamplingClient
// =============================================================================

impl SamplingClient {
    /// Construct a sampling client from a [`SamplerConfig`].
    ///
    /// Grabs the process-wide shared `reqwest::Client` (HTTP/2 by
    /// default, HTTP/1.1 when `config.force_http1` is set) and
    /// pre-computes the default request headers. This does not perform
    /// any network I/O.
    pub fn new(config: SamplerConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref api_key) = config.api_key {
            match config.auth_scheme {
                AuthScheme::XApiKey => {
                    let header_value = HeaderValue::from_str(api_key).map_err(|_| {
                        tracing::debug!(
                            api_key = %api_key,
                            "Invalid api_key: cannot be converted to a valid HTTP header"
                        );
                        SamplingError::auth_unknown(
                            "Invalid api_key: cannot be converted to a valid HTTP header",
                        )
                    })?;
                    headers.insert(HeaderName::from_static("x-api-key"), header_value);
                }
                AuthScheme::GoogleApiKey => {
                    let header_value = HeaderValue::from_str(api_key).map_err(|_| {
                        tracing::debug!(
                            api_key = %api_key,
                            "Invalid api_key: cannot be converted to a valid HTTP header"
                        );
                        SamplingError::auth_unknown(
                            "Invalid api_key: cannot be converted to a valid HTTP header",
                        )
                    })?;
                    headers.insert(HeaderName::from_static("x-goog-api-key"), header_value);
                }
                AuthScheme::Bearer => {
                    let bearer = format!("Bearer {}", api_key);
                    let header_value = HeaderValue::from_str(&bearer).map_err(|_| {
                        tracing::debug!(
                            api_key = %api_key,
                            "Invalid api_key: cannot be converted to a valid HTTP Authorization header"
                        );
                        SamplingError::auth_unknown(
                            "Invalid api_key: cannot be converted to a valid HTTP Authorization header",
                        )
                    })?;
                    headers.insert(AUTHORIZATION, header_value);
                }
            }
        }

        // Apply all extra headers verbatim. This is the single
        // injection point for proxy-auth headers and any other URL- or
        // environment-specific headers the session decides to set.
        for (key, value) in &config.extra_headers {
            let header_name = HeaderName::try_from(key.as_str())
                .map_err(|_| SamplingError::InvalidConfiguration("Invalid extra header name"))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|_| SamplingError::InvalidConfiguration("Invalid extra header value"))?;
            headers.insert(header_name, header_value);
        }

        // Resolve here, not into `extra_headers`, so an env-sourced secret stays
        // out of persisted state.
        apply_env_http_headers(
            &config.env_http_headers,
            |var| std::env::var(var).ok(),
            &mut headers,
        );

        // Add x-grok-client-version header for version gating at the proxy.
        if let Some(client_version) = config.client_version.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(client_version)
        {
            headers.insert(
                HeaderName::from_static("x-grok-client-version"),
                header_value,
            );
        }

        if let Some(deployment_id) = config.deployment_id.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(deployment_id)
        {
            headers.insert(
                HeaderName::from_static("x-grok-deployment-id"),
                header_value,
            );
        }

        if let Some(user_id) = config.user_id.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(user_id)
        {
            headers.insert(HeaderName::from_static("x-grok-user-id"), header_value);
        }

        {
            let client_id = config
                .client_identifier
                .clone()
                .unwrap_or_else(|| DEFAULT_CLIENT_IDENTIFIER.to_string());
            if let Ok(header_value) = HeaderValue::from_str(&client_id) {
                headers.insert(
                    HeaderName::from_static("x-grok-client-identifier"),
                    header_value,
                );
            }
        }

        // Always set User-Agent: per-session origin if available, else fallback.
        {
            let ua_string = match config.origin_client.as_ref() {
                Some(origin) => user_agent_string_for(origin),
                None => user_agent_string_for(&OriginClientInfo {
                    product: AGENT_PRODUCT.to_string(),
                    version: Some(agent_version()),
                }),
            };
            if let Ok(v) = HeaderValue::from_str(&ua_string) {
                headers.insert(USER_AGENT, v);
            }
        }

        let http = if let Some(proxy) = config.proxy.as_deref() {
            // A config-derived proxy needs a dedicated (non-shared) client so it
            // never contaminates the process-wide shared clients.
            tracing::info!(
                force_http1 = config.force_http1,
                "Using a dedicated proxied sampling client"
            );
            crate::shared_http::client_with_proxy(proxy, config.force_http1)
                .map_err(SamplingError::Http)?
        } else if config.force_http1 {
            tracing::info!("Using HTTP/1.1 for sampling client (force_http1=true)");
            crate::shared_http::client_http1().map_err(SamplingError::Http)?
        } else {
            crate::shared_http::client().map_err(SamplingError::Http)?
        };

        tracing::info!(
            target: crate::sampling_log::TARGET,
            event = "client_new",
            base_url = %config.base_url,
            model = %config.model,
            api_backend = ?config.api_backend,
            auth_scheme = ?config.auth_scheme,
            // "unset" (not "none"): `ReasoningEffort::None` is a real wire value;
            // logging the absent Option as "none" looked like we were sending it.
            reasoning_effort = config.reasoning_effort.map_or("unset", |e| e.as_str()),
            has_api_key = config.api_key.is_some(),
            has_bearer_resolver = config.bearer_resolver.is_some(),
            has_authorization_header = headers.get(AUTHORIZATION).is_some(),
            has_x_api_key_header = headers.get(HeaderName::from_static("x-api-key")).is_some(),
        );

        let defaults = ClientDefaults {
            model: config.model,
            max_completion_tokens: config.max_completion_tokens,
            temperature: config.temperature,
            top_p: config.top_p,
            api_backend: config.api_backend,
            auth_scheme: config.auth_scheme,
            stream_tool_calls: config.stream_tool_calls,
            doom_loop_recovery: config.doom_loop_recovery,
            thinking: config.thinking,
            max_concurrent: config.max_concurrent,
            concurrency_class: config.concurrency_class,
        };

        let endpoint = EndpointTemplate::new(&config.base_url, &config.query_params);

        Ok(Self {
            http,
            default_headers: headers,
            base_url: config.base_url,
            defaults,
            attribution_callback: config.attribution_callback,
            bearer_resolver: config.bearer_resolver,
            header_injector: config.header_injector,
            request_interceptor: config.request_interceptor,
            error_hook: config.error_hook,
            endpoint,
        })
    }

    /// The configured API backend for this client.
    pub fn api_backend(&self) -> ApiBackend {
        self.defaults.api_backend.clone()
    }

    /// A copy of this client whose requests queue as background work.
    ///
    /// Only the call site knows whether a turn is waiting on its request, so
    /// the lane is chosen here rather than inferred from the model: a session
    /// title, a prompt suggestion and memory consolidation are fire-and-forget,
    /// while an image description or a permission classification on the very
    /// same models is holding a turn open.
    pub fn as_background(&self) -> Self {
        let mut clone = self.clone();
        clone.defaults.concurrency_class = ConcurrencyClass::Background;
        clone
    }

    /// Wait for a slot on this endpoint's declared `max_concurrent` before a
    /// request goes on the wire.
    ///
    /// `model` is the *wire* model of the request being sent, not the client
    /// default, because a per-request override targets a different slug on the
    /// same endpoint and the provider meters the two separately.
    ///
    /// The permit must be bound to a local (or moved into the returned stream)
    /// and never leaked: dropping it is the only release path, and that is what
    /// makes cancellation and every error arm below correct without any of them
    /// mentioning it.
    async fn admit(&self, model: &str) -> Result<Option<AdmissionPermit>> {
        admit(
            &self.base_url,
            model,
            self.defaults.max_concurrent,
            self.defaults.concurrency_class,
        )
        .await
    }

    /// POST with default headers, returning the builder coupled to the tail
    /// fragment of the credential actually placed in its headers (`None` =
    /// no credential) — captured at build time because a record-time re-read
    /// races with the recovery a 401 triggers.
    ///
    /// A wired bearer_resolver is the sole auth source: a missing live bearer
    /// strips default Authorization / x-api-key so a hard-expired seed key
    /// cannot ride on the wire.
    fn post(&self, url: impl reqwest::IntoUrl) -> SentRequest {
        self.post_with_headers(url, None)
    }

    /// POST with a base set of non-auth headers. `base_non_auth` is `None`
    /// for the common path (use the construction-time default headers) and
    /// `Some(map)` when a [`crate::intercept::RequestInterceptor`] replaced
    /// the non-auth headers wholesale. Either way, the construction-time
    /// credentials are (re-)attached here and the live bearer resolver — when
    /// wired, the sole auth source — is applied on top, so an interceptor can
    /// never drop or forge auth.
    fn post_with_headers(
        &self,
        url: impl reqwest::IntoUrl,
        base_non_auth: Option<HeaderMap>,
    ) -> SentRequest {
        let mut headers = match base_non_auth {
            Some(mut replaced) => {
                if let Some(a) = self.default_headers.get(AUTHORIZATION) {
                    replaced.insert(AUTHORIZATION, a.clone());
                }
                let x_api_key = HeaderName::from_static("x-api-key");
                if let Some(k) = self.default_headers.get(&x_api_key) {
                    replaced.insert(x_api_key, k.clone());
                }
                replaced
            }
            None => self.default_headers.clone(),
        };
        if let Some(resolver) = &self.bearer_resolver {
            headers.remove(AUTHORIZATION);
            headers.remove(HeaderName::from_static("x-api-key"));
            headers.remove(HeaderName::from_static("x-goog-api-key"));
            if let Some(fresh) = resolver.current_bearer() {
                match self.defaults.auth_scheme {
                    AuthScheme::XApiKey => {
                        if let Ok(v) = HeaderValue::from_str(&fresh) {
                            headers.insert(HeaderName::from_static("x-api-key"), v);
                        }
                    }
                    AuthScheme::GoogleApiKey => {
                        if let Ok(v) = HeaderValue::from_str(&fresh) {
                            headers.insert(HeaderName::from_static("x-goog-api-key"), v);
                        }
                    }
                    AuthScheme::Bearer => {
                        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {fresh}")) {
                            headers.insert(AUTHORIZATION, v);
                        }
                    }
                }
            }
        }
        {
            let auth_prefix = headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(20).collect::<String>());
            let x_api_key_prefix = headers
                .get(HeaderName::from_static("x-api-key"))
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(12).collect::<String>());
            tracing::info!(
                target: crate::sampling_log::TARGET,
                event = "client_post",
                base_url = %self.base_url,
                model = %self.defaults.model,
                api_backend = ?self.defaults.api_backend,
                auth_scheme = ?self.defaults.auth_scheme,
                has_bearer_resolver = self.bearer_resolver.is_some(),
                has_authorization_header = headers.get(AUTHORIZATION).is_some(),
                has_x_api_key_header = headers.get(HeaderName::from_static("x-api-key")).is_some(),
                auth_header_prefix = auth_prefix.as_deref().unwrap_or("none"),
                x_api_key_prefix = x_api_key_prefix.as_deref().unwrap_or("none"),
            );
        }
        let sent_bearer = Self::sent_fragment_from_headers(&headers, &self.defaults.auth_scheme);
        if let Some(injector) = &self.header_injector {
            injector.inject(&mut headers);
        }
        SentRequest {
            builder: self.http.post(url).headers(headers),
            sent_bearer,
        }
    }

    /// Whether an outbound request interceptor is wired. Call sites use this
    /// to keep the zero-overhead fast path (no body serialization round-trip)
    /// when nothing is listening.
    fn has_request_interceptor(&self) -> bool {
        self.request_interceptor.is_some()
    }

    /// The construction-time headers with the credential headers removed,
    /// as owned string pairs for a [`crate::intercept::RequestView`].
    fn non_auth_headers_vec(&self) -> Vec<(String, String)> {
        let x_api_key = HeaderName::from_static("x-api-key");
        self.default_headers
            .iter()
            .filter(|(name, _)| *name != AUTHORIZATION && **name != x_api_key)
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_string(), v.to_string()))
            })
            .collect()
    }

    /// Run the wired request interceptor over `body`. Returns the
    /// (possibly replaced) body plus, when the interceptor rewrote them, the
    /// non-auth headers to send. Only call this when
    /// [`Self::has_request_interceptor`] is true; if no interceptor is wired
    /// it is a no-op that returns the body unchanged.
    async fn run_request_interceptor(
        &self,
        endpoint_path: &str,
        body: serde_json::Value,
    ) -> (serde_json::Value, Option<HeaderMap>) {
        let Some(interceptor) = self.request_interceptor.clone() else {
            return (body, None);
        };
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(self.defaults.model.as_str())
            .to_string();
        let view = crate::intercept::RequestView {
            endpoint: endpoint_path.to_string(),
            model,
            base_url_alias: self.base_url.clone(),
            headers: self.non_auth_headers_vec(),
            body,
        };
        let replacement = interceptor.intercept(&view).await;
        // Reclaim the original body now that the borrow of `view` has ended,
        // so a passthrough (no body replacement) reuses it without a clone.
        let crate::intercept::RequestView { mut body, .. } = view;
        let Some(replacement) = replacement else {
            return (body, None);
        };
        if let Some(new_body) = replacement.body {
            body = new_body;
        }
        if let Some(new_model) = replacement.model
            && let Some(obj) = body.as_object_mut()
        {
            obj.insert("model".to_string(), serde_json::Value::String(new_model));
        }
        let headers = replacement.headers.map(|pairs| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                // Skip credential headers (defense in depth: the view never
                // exposed them, but a hostile replacement must not smuggle
                // auth in) and any header that fails to parse.
                let lower = name.to_ascii_lowercase();
                if lower == "authorization" || lower == "x-api-key" {
                    continue;
                }
                if let (Ok(n), Ok(v)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(&value),
                ) {
                    map.insert(n, v);
                }
            }
            map
        });
        (body, headers)
    }

    /// Consult the wired error hook, if any, for a directive on a
    /// provider/stream error. Returns [`crate::intercept::ErrorDirective::Passthrough`]
    /// when no hook is wired. The client acts only on `Fail`; the caller owns
    /// model/base-URL substitution.
    pub(crate) async fn consult_error_hook(
        &self,
        err: &SamplingError,
        attempt: u32,
    ) -> crate::intercept::ErrorDirective {
        let Some(hook) = self.error_hook.clone() else {
            return crate::intercept::ErrorDirective::Passthrough;
        };
        let view = crate::intercept::ErrorView {
            error_class: crate::retry::classify_error_class(err).to_string(),
            model: self.defaults.model.clone(),
            base_url_alias: self.base_url.clone(),
            attempt,
        };
        hook.on_error(&view).await
    }

    /// Tail fragment of the credential in `headers` — `x-api-key`
    /// (Messages-API scheme), `x-goog-api-key` (Gemini) or `Authorization` —
    /// per [`crate::attribution::BEARER_SUFFIX_LEN`].
    fn sent_fragment_from_headers(headers: &HeaderMap, scheme: &AuthScheme) -> Option<String> {
        let raw = match scheme {
            AuthScheme::XApiKey => headers
                .get(HeaderName::from_static("x-api-key"))
                .and_then(|v| v.to_str().ok()),
            AuthScheme::GoogleApiKey => headers
                .get(HeaderName::from_static("x-goog-api-key"))
                .and_then(|v| v.to_str().ok()),
            AuthScheme::Bearer => headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer ")),
        };
        raw.map(|s| bearer_suffix(s).to_string())
    }

    /// Best-effort *build-time* view of what the next request would carry
    /// (resolver-authoritative). For request-start diagnostics
    /// ([`Self::auth_info`]) only — 401 attribution must use the fragment
    /// captured by [`Self::post`] instead, which cannot race a recovery.
    fn current_sent_bearer_suffix(&self) -> Option<String> {
        if self.bearer_resolver.is_some() {
            return self
                .bearer_resolver
                .as_ref()
                .and_then(|r| r.current_bearer())
                .map(|s| bearer_suffix(&s).to_string());
        }
        Self::sent_fragment_from_headers(&self.default_headers, &self.defaults.auth_scheme)
    }

    /// Invoke the optional 401 attribution callback for one logical
    /// 401 response. Each of the six UNAUTHORIZED arms in this file
    /// calls this helper immediately before returning
    /// `SamplingError::Auth(...)`. Emit happens at the lowest layer
    /// that saw the status, so higher layers that react to a 401 must
    /// not emit a duplicate event.
    ///
    /// `sent_suffix` is the fragment [`Self::post`] captured for the
    /// rejected request (already tail-truncated; the full bearer never
    /// crosses this boundary).
    fn record_401_attribution(
        &self,
        consumer: crate::attribution::SamplingConsumer,
        sent_suffix: Option<&str>,
    ) {
        if let Some(cb) = self.attribution_callback.as_ref() {
            cb.record_401(consumer, sent_suffix);
        }
    }

    pub fn auth_info(&self) -> crate::sampling_log::AuthInfo {
        let auth_prefix = self.current_sent_bearer_suffix();
        let auth_type = match (&self.defaults.auth_scheme, &auth_prefix) {
            (AuthScheme::XApiKey, Some(_)) => "x-api-key",
            (AuthScheme::GoogleApiKey, Some(_)) => "x-goog-api-key",
            (AuthScheme::Bearer, Some(_)) => "bearer",
            (_, None) => "none",
        };
        crate::sampling_log::AuthInfo {
            auth_type,
            auth_prefix,
        }
    }

    /// Check if a header name contains sensitive information that should be redacted.
    fn is_sensitive_header(name: &str) -> bool {
        let lower = name.to_lowercase();
        lower.contains("authorization")
            || lower.contains("api-key")
            || lower.contains("apikey")
            || lower.contains("token")
            || lower.contains("secret")
    }

    /// Short lossy body snippet for error logs (never user-facing).
    fn body_preview(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).chars().take(500).collect()
    }

    /// Log all headers from a request at debug level (redacting sensitive values).
    fn log_request_headers(request: &reqwest::Request, endpoint_name: &str) {
        for (name, value) in request.headers().iter() {
            let value_str = if Self::is_sensitive_header(name.as_str()) {
                "[REDACTED]"
            } else {
                value.to_str().unwrap_or("[non-utf8]")
            };
            tracing::debug!(
                header_name = %name,
                header_value = %value_str,
                "Request header ({})",
                endpoint_name
            );
        }
    }

    fn endpoint(&self, path: &str) -> String {
        self.endpoint.url_for_path(path)
    }

    fn apply_defaults(&self, mut request: ChatCompletionRequest) -> Result<ChatCompletionRequest> {
        if request.model.is_none() {
            request.model = Some(self.defaults.model.clone());
        }

        if request.max_tokens.is_none() {
            request.max_tokens = self.defaults.max_completion_tokens;
        }

        if request.temperature.is_none() {
            request.temperature = self.defaults.temperature;
        }

        if request.top_p.is_none() {
            request.top_p = self.defaults.top_p;
        }

        Ok(request)
    }

    /// `sent_bearer` is the fragment [`Self::post`] captured for the
    /// request that produced `response` (401 attribution).
    async fn handle_response(
        &self,
        response: reqwest::Response,
        sent_bearer: Option<&str>,
    ) -> Result<ChatCompletionResponse> {
        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = response.bytes().await?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ChatCompletions,
                    sent_bearer,
                );
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401): {server_message}"),
                    sent_bearer,
                ));
            }
            let message = user_facing_api_error_message(status, bytes.as_ref());
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let completion = serde_json::from_slice::<ChatCompletionResponse>(&bytes).map_err(|e| {
            let raw_body = String::from_utf8_lossy(&bytes);
            tracing::error!(
                error = %e,
                raw_body = %raw_body,
                "Failed to deserialize ChatCompletionResponse"
            );
            SamplingError::Serialization(e)
        })?;
        Ok(completion)
    }

    // =========================================================================
    // Chat Completions API
    // =========================================================================

    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let payload = self.apply_defaults(request)?;
        let x_grok_conv_id = &payload.x_grok_conv_id.clone().unwrap_or_default();
        let x_grok_req_id = &payload.x_grok_req_id.clone().unwrap_or_default();
        let model_id = payload.model.clone().unwrap_or_default();

        tracing::debug!(
            base_url = %self.base_url,
            model_id = %model_id,
            "Sending chat completion request"
        );

        // Held to the end of the function, so every `?` below and any drop of
        // this future returns the slot.
        let _permit = self.admit(&model_id).await?;

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: payload.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: payload.x_grok_turn_idx.as_deref(),
            agent_id: payload.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: payload.x_grok_deployment_id.as_deref(),
            user_id: payload.x_grok_user_id.as_deref(),
        };
        let endpoint = self.endpoint("chat/completions");
        let (http_request, sent_bearer) = if self.has_request_interceptor() {
            let body = serde_json::to_value(&payload).map_err(SamplingError::Serialization)?;
            let (body, hdrs) = self.run_request_interceptor("chat/completions", body).await;
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post_with_headers(endpoint, hdrs);
            (grok_headers.apply(builder).json(&body), sent_bearer)
        } else {
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post(endpoint);
            (grok_headers.apply(builder).json(&payload), sent_bearer)
        };

        let response = http_request.send().await.map_err(|e| {
            // Log at debug level; errors are surfaced to the caller.
            tracing::debug!("HTTP request failed: {}", e);
            e
        })?;

        self.handle_response(response, sent_bearer.as_deref()).await
    }

    /// Start a streaming chat completion request. Returns a stream of typed chunks.
    #[tracing::instrument(
        name = "http.chat_completion_stream",
        skip_all,
        fields(
            endpoint = %self.endpoint("chat/completions"),
            model_id = request.model.as_deref().unwrap_or(""),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<(
        BoxStream<'static, Result<ChatCompletionChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        let payload = self.apply_defaults(request)?;
        let x_grok_conv_id = &payload.x_grok_conv_id.clone().unwrap_or_default();
        let x_grok_req_id = &payload.x_grok_req_id.clone().unwrap_or_default();
        let model_id = payload.model.clone().unwrap_or_default();

        // Acquired before the body goes out and moved into the returned stream
        // below: the provider counts this request as in flight until the body
        // ends, not until its headers arrive.
        let permit = self.admit(&model_id).await?;

        // Wrap the request with streaming fields and serialize once.
        // Previously this path serialized twice: first to serde_json::Value
        // (to inject `stream` and `stream_options`), then to HTTP body bytes.
        let streaming_request = StreamingChatRequest {
            inner: &payload,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: payload.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: payload.x_grok_turn_idx.as_deref(),
            agent_id: payload.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: payload.x_grok_deployment_id.as_deref(),
            user_id: payload.x_grok_user_id.as_deref(),
        };
        let endpoint = self.endpoint("chat/completions");
        let (http_request, sent_bearer) = if self.has_request_interceptor() {
            let body =
                serde_json::to_value(&streaming_request).map_err(SamplingError::Serialization)?;
            let (body, hdrs) = self.run_request_interceptor("chat/completions", body).await;
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post_with_headers(endpoint, hdrs);
            (
                grok_headers
                    .apply(builder)
                    .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
                    .json(&body),
                sent_bearer,
            )
        } else {
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post(endpoint);
            (
                grok_headers
                    .apply(builder)
                    .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
                    .json(&streaming_request),
                sent_bearer,
            )
        };

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;

        tracing::debug!(
            url = %built_request.url(),
            method = %built_request.method(),
            "Sending chat/completions request"
        );
        Self::log_request_headers(&built_request, "chat/completions");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ChatCompletionsStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("chat/completions");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "chat/completions API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        // Strip UTF-8 BOM if present: eventsource-stream 0.2.3 incorrectly slices BOM at byte 1 instead of 3.
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        // Map SSE events into ChatCompletionChunk.
        // Uses `scan` so that `[DONE]` and transport errors both terminate the
        // stream (`None`). The first transport error is emitted to the consumer,
        // then subsequent polls return `None` -- preventing an infinite busy-loop
        // when the HTTP/2 connection drops and h2 keeps producing errors.
        let chunks = event_stream
            .scan(false, |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = &event.data;
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }

                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "chat_completions",
                            data = %data,
                        );

                        if let Some(stream_error) = try_parse_stream_error(data) {
                            Some(Err(stream_error))
                        } else {
                            Some(
                                serde_json::from_str::<ChatCompletionChunk>(data).map_err(|e| {
                                    tracing::error!(
                                        error = %e,
                                        raw_data = %data,
                                        "Failed to deserialize ChatCompletionChunk from stream"
                                    );
                                    SamplingError::Serialization(e)
                                }),
                            )
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Err(SamplingError::EventStreamError(e.to_string())))
                    }
                };
                std::future::ready(item)
            })
            .boxed();

        Ok((hold_permit_for_stream(chunks, permit), model_metadata))
    }

    // =========================================================================
    // Responses API
    // =========================================================================

    /// Apply default configuration to a Responses API request.
    fn apply_response_defaults(&self, request: &mut CreateResponseWrapper) -> Result<()> {
        // Apply model default if not specified
        if request.inner.model.is_none() {
            request.inner.model = Some(self.defaults.model.clone());
        }

        // Apply temperature default if not specified
        if request.inner.temperature.is_none() {
            request.inner.temperature = self.defaults.temperature;
        }

        // Apply top_p default if not specified
        if request.inner.top_p.is_none() {
            request.inner.top_p = self.defaults.top_p;
        }

        // Apply max_output_tokens default if not specified
        if request.inner.max_output_tokens.is_none() {
            request.inner.max_output_tokens = self.defaults.max_completion_tokens;
        }

        // Set store to false if not specified (default is true, but that breaks ZDR compliance)
        if request.inner.store.is_none() {
            request.inner.store = Some(false);
        }

        // Include encrypted reasoning content if not specified
        let includes = request.inner.include.get_or_insert_with(Vec::new);
        if !includes.contains(&rs::IncludeEnum::ReasoningEncryptedContent) {
            includes.push(rs::IncludeEnum::ReasoningEncryptedContent);
        }

        Ok(())
    }

    /// Create a response using the Responses API (non-streaming).
    ///
    /// This uses the Responses API format which provides a simpler interface
    /// for multi-turn conversations and tool calling.
    pub async fn create_response(
        &self,
        mut request: CreateResponseWrapper,
    ) -> Result<rs::Response> {
        self.apply_response_defaults(&mut request)?;

        let x_grok_conv_id = request.x_grok_conv_id.as_deref().unwrap_or_default();
        let x_grok_req_id = request.x_grok_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone().unwrap_or_default();

        // The trace field is process-local: it is consumed by upstream
        // session code (which may upload a payload artifact) and is not
        // forwarded by the sampler. Drop it before we send.
        request.trace.take();

        tracing::debug!("create_response: {:?}", &request);
        tracing::debug!("endpoint: {:?}", self.endpoint("responses"));

        // Held to the end of the function, so every `?` below and any drop of
        // this future returns the slot.
        let _permit = self.admit(&model_id).await?;

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: request.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: request.x_grok_turn_idx.as_deref(),
            agent_id: request.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_grok_deployment_id.as_deref(),
            user_id: request.x_grok_user_id.as_deref(),
        };
        let mut request_body = serde_json::to_value(&request.inner).map_err(|e| {
            tracing::error!("Failed to serialize responses request: {}", e);
            SamplingError::Serialization(e)
        })?;
        // async-openai's ReasoningTextContent struct omits the `type`
        // discriminator that the Responses API requires on input. Patch
        // it in post-serialize. This is the last surviving piece of the
        // old raw_output machinery.
        xai_grok_sampling_types::patch_reasoning_text_types(&mut request_body);
        // Body is already a JSON `Value` here, so the interceptor helper adds
        // no serialization cost on the fast path (it returns immediately when
        // no interceptor is wired).
        let (request_body, hdrs) = self
            .run_request_interceptor("responses", request_body)
            .await;
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post_with_headers(self.endpoint("responses"), hdrs);
        let http_request = grok_headers.apply(builder).json(&request_body);

        let response = http_request.send().await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            e
        })?;

        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = response.bytes().await?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::Responses,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("responses");
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let message = user_facing_api_error_message(status, bytes.as_ref());
            tracing::warn!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "responses API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let response_obj = serde_json::from_slice::<rs::Response>(&bytes).map_err(|e| {
            let raw_body = String::from_utf8_lossy(&bytes);
            tracing::error!(
                error = %e,
                raw_body = %raw_body,
                "Failed to deserialize rs::Response"
            );
            SamplingError::Serialization(e)
        })?;
        Ok(response_obj)
    }

    /// Create a streaming response using the Responses API.
    ///
    /// Returns a stream of `rs::ResponseStreamEvent` which includes events like:
    /// - `response.created` - Initial response object
    /// - `response.output_text.delta` - Text content deltas
    /// - `response.function_call_arguments.delta` - Function call argument deltas
    /// - `response.completed` - Final response with all output
    ///
    /// The third tuple element is a per-request doom-loop signal collector,
    /// `Some` only when `SamplerConfig::doom_loop_recovery` is set — the same
    /// gate that adds the opt-in `x-grok-doom-loop-check` request header, so
    /// header and parse protection cannot drift apart. It is filled by the
    /// SSE decoder as the server reports triggers and is meant to be handed
    /// to `stream_responses` so the signals land on the final
    /// `ConversationResponse`.
    #[tracing::instrument(
        name = "http.create_response_stream",
        skip_all,
        fields(
            endpoint = %self.endpoint("responses"),
            model_id = request.inner.model.as_deref().unwrap_or(""),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    #[allow(clippy::type_complexity)]
    pub async fn create_response_stream(
        &self,
        mut request: CreateResponseWrapper,
    ) -> Result<(
        BoxStream<'static, Result<rs::ResponseStreamEvent>>,
        Option<ResponseModelMetadata>,
        Option<crate::doom_loop::DoomLoopSignalCollector>,
    )> {
        self.apply_response_defaults(&mut request)?;

        // Enable streaming
        request.inner.stream = Some(true);

        let x_grok_conv_id = request.x_grok_conv_id.as_deref().unwrap_or_default();
        let x_grok_req_id = request.x_grok_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone().unwrap_or_default();

        // Drop process-local trace data (see note in `create_response`).
        request.trace.take();

        tracing::debug!(
            base_url = %self.base_url,
            model_id = model_id.as_str(),
            "Sending responses API stream request"
        );

        // Acquired before the body goes out and moved into the returned stream
        // below: the provider counts this request as in flight until the body
        // ends, not until its headers arrive.
        let permit = self.admit(&model_id).await?;

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: request.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: request.x_grok_turn_idx.as_deref(),
            agent_id: request.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_grok_deployment_id.as_deref(),
            user_id: request.x_grok_user_id.as_deref(),
        };
        let extra_tool_entries = std::mem::take(&mut request.extra_tool_entries);
        let mut request_body = serde_json::to_value(&request.inner).map_err(|e| {
            tracing::error!("Failed to serialize responses request: {}", e);
            SamplingError::Serialization(e)
        })?;
        // Inject xAI-specific fields not in async-openai's CreateResponse type.
        if self.defaults.stream_tool_calls {
            request_body["stream_tool_calls"] = serde_json::json!(true);
        }
        // Inject xAI-specific tools (e.g., x_search) that can't be expressed
        // via async_openai's rs::Tool enum.
        if !extra_tool_entries.is_empty() {
            if let Some(tools) = request_body.get_mut("tools").and_then(|v| v.as_array_mut()) {
                tools.extend(extra_tool_entries);
            } else {
                request_body["tools"] = serde_json::Value::Array(extra_tool_entries);
            }
        }
        xai_grok_sampling_types::patch_reasoning_text_types(&mut request_body);
        // Fresh per attempt so signals never leak across retries; `None`
        // (check disabled) sends no header and does no peek work per event.
        let doom_loop = self
            .defaults
            .doom_loop_recovery
            .map(crate::doom_loop::DoomLoopSignalCollector::new);
        let (request_body, hdrs) = self
            .run_request_interceptor("responses", request_body)
            .await;
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post_with_headers(self.endpoint("responses"), hdrs);
        let mut http_request = grok_headers
            .apply(builder)
            .header(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if doom_loop.is_some() {
            // Presence opts in; the server ignores the value.
            http_request = http_request.header(DOOM_LOOP_CHECK_HEADER, "true");
        }
        let http_request = http_request.json(&request_body);

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;

        tracing::debug!(
            url = %built_request.url(),
            method = %built_request.method(),
            "Sending responses API stream request"
        );
        Self::log_request_headers(&built_request, "responses");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ResponsesStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("responses");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }
            let model_metadata = extract_model_metadata(response.headers());
            let retry_after_secs = extract_retry_after(response.headers());
            let should_retry = extract_should_retry(response.headers());
            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "responses API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let model_metadata = extract_model_metadata(response.headers());

        // Strip UTF-8 BOM if present
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        let doom_loop_for_stream = doom_loop.clone();

        // The scan item is an `Option`: `Some(None)` skips an absorbed
        // doom-loop event without terminating the stream (`filter_map`
        // below), while an outer `None` still ends it.
        let events = event_stream
            .scan(false, move |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = &event.data;
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }

                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "responses",
                            data = %data,
                        );

                        // Intercept the non-standard doom-loop event before
                        // typed deserialization; async-openai's event enum
                        // does not know it and would fail to parse it. With
                        // the check disabled, the shared name-or-payload-type
                        // predicate guards against a server emitting it
                        // despite no opt-in (rollout skew), named or not.
                        let swallow = match &doom_loop_for_stream {
                            Some(collector) => collector.absorb(&event.event, data),
                            None => is_check_event(&event.event, data),
                        };
                        if swallow {
                            Some(None)
                        } else if let Some(stream_error) = try_parse_stream_error(data) {
                            Some(Some(Err(stream_error)))
                        } else {
                            // `Ok(None)` is a frame the decoder skipped
                            // (unrecognized event type); like an absorbed
                            // doom-loop event it drops out of the stream
                            // without ending it.
                            match deserialize_response_event(data) {
                                Ok(Some(event)) => Some(Some(Ok(event))),
                                Ok(None) => Some(None),
                                Err(err) => Some(Some(Err(err))),
                            }
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Some(Err(SamplingError::EventStreamError(e.to_string()))))
                    }
                };
                std::future::ready(item)
            })
            .filter_map(std::future::ready)
            .boxed();

        Ok((
            hold_permit_for_stream(events, permit),
            model_metadata,
            doom_loop,
        ))
    }

    // =========================================================================
    // Anthropic Messages API
    // =========================================================================

    /// Apply default configuration to a Messages API request.
    ///
    /// Fails with [`SamplingError::InvalidConfiguration`] when `max_tokens` has
    /// to be defaulted but the model config declares no ceiling to default it
    /// to; see `MISSING_MAX_COMPLETION_TOKENS`.
    fn apply_message_defaults(&self, request: &mut MessagesRequestWrapper) -> Result<()> {
        // Apply model default if not specified
        if request.inner.model.is_empty() {
            request.inner.model = self.defaults.model.clone();
        }

        // `max_tokens` is mandatory on the Messages API and must stay at or
        // below the target model's output ceiling. The declared
        // `max_completion_tokens` is the only ceiling the sampler can see, so
        // clamp to it when it exists and refuse to invent one when it does not.
        match self.defaults.max_completion_tokens {
            Some(ceiling) if request.inner.max_tokens == 0 => {
                request.inner.max_tokens = ceiling;
            }
            Some(ceiling) if request.inner.max_tokens > ceiling => {
                tracing::warn!(
                    model = %request.inner.model,
                    requested = request.inner.max_tokens,
                    ceiling,
                    "messages: clamping max_tokens to the model's declared ceiling"
                );
                request.inner.max_tokens = ceiling;
            }
            Some(_) => {}
            None if request.inner.max_tokens == 0 => {
                // Logged with the model because the error variant carries a
                // `&'static str` and cannot name it.
                tracing::error!(
                    model = %request.inner.model,
                    "messages: model config declares no max_completion_tokens; \
                     refusing to guess a max_tokens ceiling"
                );
                return Err(SamplingError::InvalidConfiguration(
                    MISSING_MAX_COMPLETION_TOKENS,
                ));
            }
            None => {}
        }

        // Apply temperature default if not specified
        if request.inner.temperature.is_none() {
            request.inner.temperature = self.defaults.temperature;
        }

        // Apply top_p default if not specified
        if request.inner.top_p.is_none() {
            request.inner.top_p = self.defaults.top_p;
        }

        Ok(())
    }

    /// Create a message using the Anthropic Messages API (non-streaming).
    pub async fn create_message(
        &self,
        mut request: MessagesRequestWrapper,
    ) -> Result<messages::MessagesResponse> {
        self.apply_message_defaults(&mut request)?;

        let x_grok_conv_id = request.x_grok_conv_id.as_deref().unwrap_or_default();
        let x_grok_req_id = request.x_grok_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone();

        // Drop process-local trace data.
        request.trace.take();

        tracing::debug!("create_message: {:?}", &request.inner);
        tracing::debug!("endpoint: {:?}", self.endpoint("messages"));

        // Held to the end of the function, so every `?` below and any drop of
        // this future returns the slot.
        let _permit = self.admit(&model_id).await?;

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: request.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: request.x_grok_turn_idx.as_deref(),
            agent_id: request.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_grok_deployment_id.as_deref(),
            user_id: request.x_grok_user_id.as_deref(),
        };
        let endpoint = self.endpoint("messages");
        let (http_request, sent_bearer) = if self.has_request_interceptor() {
            let body =
                serde_json::to_value(&request.inner).map_err(SamplingError::Serialization)?;
            let (body, hdrs) = self.run_request_interceptor("messages", body).await;
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post_with_headers(endpoint, hdrs);
            (grok_headers.apply(builder).json(&body), sent_bearer)
        } else {
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post(endpoint);
            (grok_headers.apply(builder).json(&request.inner), sent_bearer)
        };

        let response = http_request.send().await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            e
        })?;

        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = response.bytes().await?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::Messages,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("messages");
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let message = user_facing_api_error_message(status, bytes.as_ref());
            tracing::warn!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "messages API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let response_obj =
            serde_json::from_slice::<messages::MessagesResponse>(&bytes).map_err(|e| {
                let raw_body = String::from_utf8_lossy(&bytes);
                tracing::error!(
                    error = %e,
                    raw_body = %raw_body,
                    "Failed to deserialize MessagesResponse"
                );
                SamplingError::Serialization(e)
            })?;
        Ok(response_obj)
    }

    /// Create a streaming message using the Anthropic Messages API.
    ///
    /// Returns a stream of `MessageStreamEvent` which includes events like:
    /// - `message_start` - Initial message object
    /// - `content_block_start` / `content_block_delta` / `content_block_stop` - Content blocks
    /// - `message_delta` / `message_stop` - Final message with stop reason
    #[tracing::instrument(
        name = "http.create_message_stream",
        skip_all,
        fields(
            endpoint = %self.endpoint("messages"),
            model_id = request.inner.model.as_str(),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub async fn create_message_stream(
        &self,
        mut request: MessagesRequestWrapper,
    ) -> Result<(
        BoxStream<'static, Result<messages::MessageStreamEvent>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_message_defaults(&mut request)?;

        // Enable streaming
        request.inner.stream = Some(true);

        let x_grok_conv_id = request.x_grok_conv_id.as_deref().unwrap_or_default();
        let x_grok_req_id = request.x_grok_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone();

        // Drop process-local trace data.
        request.trace.take();

        tracing::debug!(
            base_url = %self.base_url,
            model_id = model_id.as_str(),
            "Sending Messages API stream request"
        );

        // Acquired before the body goes out and moved into the returned stream
        // below: the provider counts this request as in flight until the body
        // ends, not until its headers arrive.
        let permit = self.admit(&model_id).await?;

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: request.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: request.x_grok_turn_idx.as_deref(),
            agent_id: request.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_grok_deployment_id.as_deref(),
            user_id: request.x_grok_user_id.as_deref(),
        };
        let endpoint = self.endpoint("messages");
        let (http_request, sent_bearer) = if self.has_request_interceptor() {
            let body =
                serde_json::to_value(&request.inner).map_err(SamplingError::Serialization)?;
            let (body, hdrs) = self.run_request_interceptor("messages", body).await;
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post_with_headers(endpoint, hdrs);
            (
                grok_headers
                    .apply(builder)
                    .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
                    .json(&body),
                sent_bearer,
            )
        } else {
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post(endpoint);
            (
                grok_headers
                    .apply(builder)
                    .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
                    .json(&request.inner),
                sent_bearer,
            )
        };

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;

        tracing::debug!(
            url = %built_request.url(),
            method = %built_request.method(),
            "Sending messages API stream request"
        );
        Self::log_request_headers(&built_request, "messages");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::MessagesStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("messages");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }
            let model_metadata = extract_model_metadata(response.headers());
            let retry_after_secs = extract_retry_after(response.headers());
            let should_retry = extract_should_retry(response.headers());
            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "messages API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let model_metadata = extract_model_metadata(response.headers());

        // Strip UTF-8 BOM if present
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        // Map SSE events into MessageStreamEvent.
        // Uses `scan` so transport errors terminate the stream after the first
        // error (same pattern as `chat_completion_stream`).
        let events = event_stream
            .scan(false, |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = &event.data;
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }

                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "messages",
                            data = %data,
                        );

                        if let Some(stream_error) = try_parse_stream_error(data) {
                            Some(Err(stream_error))
                        } else {
                            Some(
                                serde_json::from_str::<messages::MessageStreamEvent>(data).map_err(
                                    |e| {
                                        tracing::error!(
                                            error = %e,
                                            raw_data = %data,
                                            "Failed to deserialize MessageStreamEvent from stream"
                                        );
                                        SamplingError::Serialization(e)
                                    },
                                ),
                            )
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Err(SamplingError::EventStreamError(e.to_string())))
                    }
                };
                std::future::ready(item)
            })
            .boxed();

        Ok((hold_permit_for_stream(events, permit), model_metadata))
    }

    // =========================================================================
    // Unified Conversation API
    // =========================================================================

    /// Apply default configuration to a ConversationRequest.
    fn apply_conversation_defaults(&self, request: &mut ConversationRequest) -> Result<()> {
        if request.model.is_none() {
            request.model = Some(self.defaults.model.clone());
        }

        if request.temperature.is_none() {
            request.temperature = self.defaults.temperature;
        }

        if request.top_p.is_none() {
            request.top_p = self.defaults.top_p;
        }

        if request.max_output_tokens.is_none() {
            request.max_output_tokens = self.defaults.max_completion_tokens;
        }

        Ok(())
    }

    /// Send a conversation request using the Chat Completions API (streaming).
    ///
    /// Converts the `ConversationRequest` to `ChatCompletionRequest` internally.
    /// Returns the stream and any model metadata extracted from response headers.
    pub async fn conversation_stream(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<ChatCompletionChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let mut chat_request: ChatCompletionRequest = request.into();
        if let Some(trace) = trace {
            chat_request.trace = Some(trace);
        }

        self.chat_completion_stream(chat_request).await
    }

    /// Send a conversation request using the Chat Completions API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to `ChatCompletionRequest` internally.
    pub async fn conversation(
        &self,
        mut request: ConversationRequest,
    ) -> Result<ChatCompletionResponse> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let mut chat_request: ChatCompletionRequest = request.into();
        if let Some(trace) = trace {
            chat_request.trace = Some(trace);
        }

        self.chat_completion(chat_request).await
    }

    /// Send a conversation request using the Responses API (streaming).
    ///
    /// Converts the `ConversationRequest` to Responses API format internally.
    /// The third tuple element is the per-request doom-loop signal collector
    /// (see [`Self::create_response_stream`]); callers that don't consume the
    /// signals can ignore it.
    #[allow(clippy::type_complexity)]
    pub async fn conversation_stream_responses(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<rs::ResponseStreamEvent>>,
        Option<ResponseModelMetadata>,
        Option<crate::doom_loop::DoomLoopSignalCollector>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_grok_conv_id = request.x_grok_conv_id.clone();
        let x_grok_req_id = request.x_grok_req_id.clone();
        let x_grok_session_id = request.x_grok_session_id.clone();
        let x_grok_turn_idx = request.x_grok_turn_idx.clone();
        let x_grok_agent_id = request.x_grok_agent_id.clone();

        // Collect xAI-specific tools that can't be expressed via rs::Tool
        // (e.g., x_search). These are injected as raw JSON after serialization.
        let extra_tools = xai_grok_sampling_types::extra_tool_entries(&request.hosted_tools);

        let responses_request: rs::CreateResponse = (&request).into();

        let mut wrapper = CreateResponseWrapper::new(responses_request);
        wrapper.x_grok_conv_id = x_grok_conv_id;
        wrapper.x_grok_req_id = x_grok_req_id;
        wrapper.x_grok_session_id = x_grok_session_id;
        wrapper.x_grok_turn_idx = x_grok_turn_idx;
        wrapper.x_grok_agent_id = x_grok_agent_id;
        wrapper.extra_tool_entries = extra_tools;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_response_stream(wrapper).await
    }

    /// Send a conversation request using the Responses API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to Responses API format internally.
    pub async fn conversation_responses(
        &self,
        mut request: ConversationRequest,
    ) -> Result<rs::Response> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_grok_conv_id = request.x_grok_conv_id.clone();
        let x_grok_req_id = request.x_grok_req_id.clone();
        let x_grok_session_id = request.x_grok_session_id.clone();
        let x_grok_turn_idx = request.x_grok_turn_idx.clone();
        let x_grok_agent_id = request.x_grok_agent_id.clone();

        let responses_request: rs::CreateResponse = (&request).into();

        let mut wrapper = CreateResponseWrapper::new(responses_request);
        wrapper.x_grok_conv_id = x_grok_conv_id;
        wrapper.x_grok_req_id = x_grok_req_id;
        wrapper.x_grok_session_id = x_grok_session_id;
        wrapper.x_grok_turn_idx = x_grok_turn_idx;
        wrapper.x_grok_agent_id = x_grok_agent_id;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_response(wrapper).await
    }

    /// Send a conversation request using the Anthropic Messages API (streaming).
    ///
    /// Converts the `ConversationRequest` to Messages API format internally.
    pub async fn conversation_stream_messages(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<messages::MessageStreamEvent>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_grok_conv_id = request.x_grok_conv_id.clone();
        let x_grok_req_id = request.x_grok_req_id.clone();
        let x_grok_session_id = request.x_grok_session_id.clone();
        let x_grok_turn_idx = request.x_grok_turn_idx.clone();
        let x_grok_agent_id = request.x_grok_agent_id.clone();

        let messages_request = build_messages_request(&request, self.defaults.thinking);

        let mut wrapper = MessagesRequestWrapper::new(messages_request);
        wrapper.x_grok_conv_id = x_grok_conv_id;
        wrapper.x_grok_req_id = x_grok_req_id;
        wrapper.x_grok_session_id = x_grok_session_id;
        wrapper.x_grok_turn_idx = x_grok_turn_idx;
        wrapper.x_grok_agent_id = x_grok_agent_id;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_message_stream(wrapper).await
    }

    /// Send a conversation request using the Google Gemini API (streaming).
    ///
    /// Converts the `ConversationRequest` to Gemini format internally and
    /// streams `streamGenerateContent` SSE chunks.
    pub async fn conversation_stream_gemini(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<GeminiStreamChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_conversation_defaults(&mut request)?;
        request.trace.take();
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| self.defaults.model.clone());
        let gemini_request = build_gemini_request(&request);
        self.create_gemini_stream(model, gemini_request).await
    }

    /// POST a Gemini `streamGenerateContent` request and decode the SSE body
    /// into a stream of [`GeminiStreamChunk`]s. The model is carried in the URL
    /// path (`/models/<model>:streamGenerateContent?alt=sse`), unlike the other
    /// backends where it rides in the request body.
    #[tracing::instrument(skip(self, gemini_request), fields(status_code, success, error))]
    async fn create_gemini_stream(
        &self,
        model: String,
        gemini_request: GeminiRequest,
    ) -> Result<(
        BoxStream<'static, Result<GeminiStreamChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        let base = self.base_url.trim_end_matches('/');
        let endpoint = format!("{base}/models/{model}:streamGenerateContent?alt=sse");

        tracing::debug!(
            base_url = %self.base_url,
            model_id = %model,
            "Sending Gemini API stream request"
        );

        // Acquired before the body goes out and moved into the returned stream
        // below: the provider counts this request as in flight until the body
        // ends, not until its headers arrive.
        let permit = self.admit(&model).await?;

        let (http_request, sent_bearer) = if self.has_request_interceptor() {
            let body =
                serde_json::to_value(&gemini_request).map_err(SamplingError::Serialization)?;
            let (body, hdrs) = self.run_request_interceptor("gemini", body).await;
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post_with_headers(&endpoint, hdrs);
            (
                builder
                    .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
                    .json(&body),
                sent_bearer,
            )
        } else {
            let SentRequest {
                builder,
                sent_bearer,
            } = self.post(&endpoint);
            (
                builder
                    .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
                    .json(&gemini_request),
                sent_bearer,
            )
        };

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;
        Self::log_request_headers(&built_request, "gemini");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::MessagesStream,
                    sent_bearer.as_deref(),
                );
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }
            let model_metadata = extract_model_metadata(response.headers());
            let retry_after_secs = extract_retry_after(response.headers());
            let should_retry = extract_should_retry(response.headers());
            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model,
                "Gemini API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let model_metadata = extract_model_metadata(response.headers());

        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        let event_stream = byte_stream.eventsource();

        let events = event_stream
            .scan(false, |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = &event.data;
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }
                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "gemini",
                            data = %data,
                        );
                        if let Some(stream_error) = try_parse_stream_error(data) {
                            Some(Err(stream_error))
                        } else {
                            Some(
                                serde_json::from_str::<GeminiStreamChunk>(data).map_err(|e| {
                                    tracing::error!(
                                        error = %e,
                                        raw_data = %data,
                                        "Failed to deserialize GeminiStreamChunk from stream"
                                    );
                                    SamplingError::Serialization(e)
                                }),
                            )
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Err(SamplingError::EventStreamError(e.to_string())))
                    }
                };
                std::future::ready(item)
            })
            .boxed();

        Ok((hold_permit_for_stream(events, permit), model_metadata))
    }

    /// Send a conversation request using the Anthropic Messages API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to Messages API format internally.
    pub async fn conversation_messages(
        &self,
        mut request: ConversationRequest,
    ) -> Result<messages::MessagesResponse> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_grok_conv_id = request.x_grok_conv_id.clone();
        let x_grok_req_id = request.x_grok_req_id.clone();
        let x_grok_session_id = request.x_grok_session_id.clone();
        let x_grok_turn_idx = request.x_grok_turn_idx.clone();
        let x_grok_agent_id = request.x_grok_agent_id.clone();

        let messages_request = build_messages_request(&request, self.defaults.thinking);

        let mut wrapper = MessagesRequestWrapper::new(messages_request);
        wrapper.x_grok_conv_id = x_grok_conv_id;
        wrapper.x_grok_req_id = x_grok_req_id;
        wrapper.x_grok_session_id = x_grok_session_id;
        wrapper.x_grok_turn_idx = x_grok_turn_idx;
        wrapper.x_grok_agent_id = x_grok_agent_id;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_message(wrapper).await
    }

    /// Backend-aware streaming call that collects the full response.
    pub async fn conversation_collect(
        &self,
        request: ConversationRequest,
    ) -> Result<ConversationResponse> {
        let request_id = crate::types::RequestId::random();
        let idle_timeout = std::time::Duration::from_secs(300);
        let result = match self.api_backend() {
            ApiBackend::ChatCompletions => {
                let (raw, meta) = self.conversation_stream(request).await?;
                let events =
                    crate::stream::stream_chat_completions(raw, meta, request_id, idle_timeout);
                crate::stream::collect_response(events).await
            }
            ApiBackend::Responses => {
                let (raw, meta, doom_loop) = self.conversation_stream_responses(request).await?;
                let events =
                    crate::stream::stream_responses(raw, meta, request_id, idle_timeout, doom_loop);
                crate::stream::collect_response(events).await
            }
            ApiBackend::Messages => {
                let (raw, meta) = self.conversation_stream_messages(request).await?;
                let events = crate::stream::stream_messages(raw, meta, request_id, idle_timeout);
                crate::stream::collect_response(events).await
            }
            ApiBackend::Gemini => {
                let (raw, meta) = self.conversation_stream_gemini(request).await?;
                let events = crate::stream::stream_gemini(raw, meta, request_id, idle_timeout);
                crate::stream::collect_response(events).await
            }
        };
        result
            .map(|(response, _metrics)| response)
            .map_err(stream_collect_error)
    }
}

/// Rebuild `Api` from stream-collected info, preserving status,
/// `Retry-After`, and `x-should-retry` (kind is lost on this path).
fn stream_collect_error(info: SamplingErrorInfo) -> SamplingError {
    SamplingError::Api {
        status: info
            .status_code
            .and_then(|c| reqwest::StatusCode::from_u16(c).ok())
            .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
        message: info.message,
        model_metadata: info.model_metadata,
        retry_after_secs: info.retry_after_secs,
        should_retry: info.should_retry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use xai_grok_sampling_types::types::ChatRequestMessage;

    #[test]
    fn stream_collect_error_preserves_should_retry() {
        let info = SamplingErrorInfo {
            kind: crate::events::SamplingErrorKind::Api,
            status_code: Some(529),
            message: "Overloaded".into(),
            is_retryable: true,
            retry_after_secs: Some(3),
            should_retry: Some(false),
            model_metadata: None,
            empty_response_context: None,
            doom_loop_triggers: None,
            doom_loop_aborted_at_chunk: None,
            credential: xai_grok_sampling_types::SentCredential::Unknown,
        };
        // SamplingError is not PartialEq (it carries reqwest/serde errors),
        // so destructure once and compare all fields in a single assert.
        let SamplingError::Api {
            status,
            message,
            model_metadata,
            retry_after_secs,
            should_retry,
        } = stream_collect_error(info)
        else {
            panic!("expected Api");
        };
        assert_eq!(
            (
                status.as_u16(),
                message.as_str(),
                model_metadata.is_none(),
                retry_after_secs,
                should_retry,
            ),
            (529, "Overloaded", true, Some(3), Some(false)),
        );
    }

    fn minimal_config() -> SamplerConfig {
        SamplerConfig {
            api_key: Some("test-key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "test-model".to_string(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: ApiBackend::ChatCompletions,
            auth_scheme: AuthScheme::Bearer,
            extra_headers: IndexMap::new(),
            query_params: IndexMap::new(),
            env_http_headers: IndexMap::new(),
            context_window: 8192,
            force_http1: false,
            max_retries: None,
            stream_tool_calls: false,
            idle_timeout_secs: None,
            proxy: None,
            reasoning_effort: None,
            thinking: None,
            max_concurrent: None,
            concurrency_class: crate::concurrency::ConcurrencyClass::Interactive,
            origin_client: None,
            client_identifier: None,
            deployment_id: None,
            user_id: None,
            client_version: None,
            attribution_callback: None,
            bearer_resolver: None,
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            doom_loop_recovery: None,
            header_injector: None,
            request_interceptor: None,
            error_hook: None,
        }
    }

    /// The failure mode that would kill admission control in production: a
    /// request that has taken a slot and is then *cancelled* — its future
    /// dropped mid-flight, which is what an interrupted turn or a torn-down
    /// session does — must hand the slot back. A leak here is silent and
    /// cumulative: the endpoint simply stops being reachable after
    /// `max_concurrent` interruptions.
    #[tokio::test]
    async fn cancelling_an_in_flight_request_returns_its_slot() {
        use std::num::NonZeroUsize;
        use std::time::Duration;

        // Accepts the connection and never answers, so the request parks on the
        // wire with the slot held instead of running to completion.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _silent_server = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                open.push(socket);
            }
        });

        let base_url = format!("http://{addr}");
        let max = NonZeroUsize::new(1);
        let client = SamplingClient::new(SamplerConfig {
            base_url: base_url.clone(),
            model: "m".to_string(),
            max_concurrent: max,
            ..minimal_config()
        })
        .expect("client should build");

        let probe =
            || crate::concurrency::admit(&base_url, "m", max, ConcurrencyClass::Interactive);

        let mut in_flight = Box::pin(client.conversation_collect(
            xai_grok_sampling_types::ConversationRequest {
                items: vec![
                    xai_grok_sampling_types::conversation::ConversationItem::user("hi".to_owned()),
                ],
                model: Some("m".to_owned()),
                ..Default::default()
            },
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut in_flight)
                .await
                .is_err(),
            "the request must still be parked on the wire"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), probe())
                .await
                .is_err(),
            "an in-flight request holds the only slot"
        );

        drop(in_flight);

        assert!(
            tokio::time::timeout(Duration::from_millis(500), probe())
                .await
                .is_ok(),
            "cancelling the request future must return its slot"
        );
    }

    /// `as_background` really moves a client into the background lane, and
    /// leaves the original alone. The session's own client is cloned for
    /// fire-and-forget work such as title generation, so a version that mutated
    /// in place would demote the turn along with it.
    #[test]
    fn as_background_moves_only_the_clone_into_the_background_lane() {
        let client = SamplingClient::new(minimal_config()).expect("client should build");
        let background = client.as_background();
        assert_eq!(
            background.defaults.concurrency_class,
            ConcurrencyClass::Background
        );
        assert_eq!(
            client.defaults.concurrency_class,
            ConcurrencyClass::Interactive,
        );
    }

    /// Verify the serialized shape of StreamingChatRequest matches the
    /// expected wire format: all ChatCompletionRequest fields flattened at
    /// top level, plus `stream: true` and `stream_options.include_usage: true`.
    #[test]
    fn streaming_chat_request_serializes_correctly() {
        let request = ChatCompletionRequest {
            model: Some("test-model".into()),
            messages: vec![ChatRequestMessage::user("hello")],
            temperature: Some(0.7),
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            search_parameters: None,
            response_format: None,
            reasoning_effort: None,
            x_grok_conv_id: None,
            x_grok_req_id: None,
            x_grok_session_id: None,
            x_grok_turn_idx: None,
            x_grok_agent_id: None,
            x_grok_deployment_id: None,
            x_grok_user_id: None,
            trace: None,
        };

        let wrapper = StreamingChatRequest {
            inner: &request,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };

        let json: serde_json::Value = serde_json::to_value(&wrapper).unwrap();
        let obj = json.as_object().unwrap();

        assert_eq!(obj.get("stream").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            obj.get("stream_options")
                .and_then(|v| v.get("include_usage"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        assert!(
            obj.get("inner").is_none(),
            "inner field should be flattened"
        );
        assert_eq!(
            obj.get("model").and_then(|v| v.as_str()),
            Some("test-model")
        );
        assert!(obj.get("messages").is_some());
        let temp = obj.get("temperature").and_then(|v| v.as_f64()).unwrap();
        assert!((temp - 0.7).abs() < 0.001, "temperature should be ~0.7");

        assert!(obj.get("max_tokens").is_none());
        assert!(obj.get("tools").is_none());
    }

    #[test]
    fn extract_retry_after_parses_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(30));
    }

    #[test]
    fn extract_retry_after_caps_at_120() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(120));
    }

    #[test]
    fn extract_retry_after_zero_is_valid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "0".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(0));
    }

    #[test]
    fn extract_retry_after_ignores_http_date() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Fri, 31 Dec 2025 23:59:59 GMT".parse().unwrap(),
        );
        assert_eq!(extract_retry_after(&headers), None);
    }

    #[test]
    fn extract_retry_after_none_when_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(extract_retry_after(&headers), None);
    }

    #[test]
    fn extract_should_retry_true() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "true".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(true));
    }

    #[test]
    fn extract_should_retry_true_case_insensitive() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "TRUE".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(true));
    }

    #[test]
    fn extract_should_retry_false() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "false".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(false));
    }

    #[test]
    fn extract_should_retry_unknown_value_is_none() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "banana".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), None);
    }

    #[test]
    fn extract_should_retry_absent_is_none() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(extract_should_retry(&headers), None);
    }

    #[test]
    fn new_with_minimal_config_succeeds() {
        let client = SamplingClient::new(minimal_config()).expect("client should construct");
        assert_eq!(client.api_backend(), ApiBackend::ChatCompletions);
    }

    #[test]
    fn new_with_proxy_builds_a_dedicated_client() {
        // `reqwest::Proxy::all` validates the URL at build time, so a
        // successful construction exercises the real per-provider proxy path
        // (no network I/O).
        let mut cfg = minimal_config();
        cfg.proxy = Some("http://proxy.test:8080".to_string());
        let client = SamplingClient::new(cfg).expect("proxied client should build");
        assert_eq!(client.api_backend(), ApiBackend::ChatCompletions);
    }

    #[test]
    fn new_with_invalid_proxy_url_fails() {
        let mut cfg = minimal_config();
        cfg.proxy = Some("not a valid proxy url".to_string());
        assert!(
            SamplingClient::new(cfg).is_err(),
            "an unparseable proxy URL must fail client construction"
        );
    }

    #[test]
    fn new_applies_extra_headers() {
        let mut cfg = minimal_config();
        cfg.extra_headers
            .insert("x-test-header".to_string(), "test-value".to_string());
        cfg.extra_headers
            .insert("x-XAI-token-auth".to_string(), "xai-grok-cli".to_string());
        let _client = SamplingClient::new(cfg).expect("client with extra headers should construct");
    }

    #[test]
    fn apply_env_http_headers_resolves_trims_skips_and_overrides() {
        let mut map = IndexMap::new();
        map.insert("x-tenant-token".to_string(), "TENANT".to_string());
        map.insert("x-blank".to_string(), "BLANK".to_string());
        map.insert("x-missing".to_string(), "MISSING".to_string());
        map.insert("x-override".to_string(), "OVERRIDE".to_string());
        map.insert("x invalid".to_string(), "INVALID".to_string());

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-override"),
            HeaderValue::from_static("static"),
        );

        apply_env_http_headers(
            &map,
            |var| match var {
                // Leading space + trailing newline exercises trimming.
                "TENANT" => Some(" tenant-secret\n".to_string()),
                "BLANK" => Some("   ".to_string()),
                "OVERRIDE" => Some("from-env".to_string()),
                "INVALID" => Some("value".to_string()),
                _ => None,
            },
            &mut headers,
        );

        assert_eq!(headers.get("x-tenant-token").unwrap(), "tenant-secret");
        assert!(headers.get("x-blank").is_none());
        assert!(headers.get("x-missing").is_none());
        // A resolved env value overrides an existing header of the same name.
        assert_eq!(headers.get("x-override").unwrap(), "from-env");
        // An invalid header name is skipped rather than panicking.
        assert!(headers.get("x invalid").is_none());
    }

    #[test]
    fn endpoint_appends_path_before_a_base_url_query_without_configured_params() {
        let template =
            EndpointTemplate::new("https://gateway.example/v1?api-version=x", &IndexMap::new());
        let url = template.url_for_path("responses");
        assert!(
            url.starts_with("https://gateway.example/v1/responses?"),
            "url: {url}"
        );
        assert!(url.contains("api-version=x"), "url: {url}");
        assert!(!url.contains("x/responses"), "url: {url}");
    }

    #[test]
    fn messages_plus_anthropic_api_key_uses_x_api_key_and_not_authorization() {
        let cfg = SamplerConfig {
            api_key: Some("anthropic-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert!(
            client
                .default_headers
                .get(HeaderName::from_static("x-api-key"))
                .is_some()
        );
        assert!(client.default_headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn messages_plus_bearer_uses_authorization_and_not_x_api_key() {
        let cfg = SamplerConfig {
            api_key: Some("bearer-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::Bearer,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert!(client.default_headers.get(AUTHORIZATION).is_some());
        assert!(
            client
                .default_headers
                .get(HeaderName::from_static("x-api-key"))
                .is_none()
        );
    }

    fn messages_client(max_completion_tokens: Option<u32>) -> SamplingClient {
        SamplingClient::new(SamplerConfig {
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            model: "example-model".to_string(),
            max_completion_tokens,
            ..minimal_config()
        })
        .expect("client should build")
    }

    fn messages_request(max_tokens: u32) -> MessagesRequestWrapper {
        MessagesRequestWrapper::new(messages::MessagesRequest {
            max_tokens,
            ..Default::default()
        })
    }

    /// Regression: a model whose declared ceiling is below the old built-in
    /// 128K default must never be sent a larger `max_tokens` -- Anthropic
    /// answers that with a 400 that reads like a generic request error.
    #[test]
    fn messages_defaults_clamp_max_tokens_to_declared_ceiling() {
        let client = messages_client(Some(64_000));

        // Defaulted (`max_tokens == 0`) resolves to the declared ceiling.
        let mut defaulted = messages_request(0);
        client
            .apply_message_defaults(&mut defaulted)
            .expect("declared ceiling is enough to default max_tokens");
        assert_eq!(defaulted.inner.max_tokens, 64_000);

        // A caller-supplied value above the ceiling is clamped down to it.
        let mut over = messages_request(128_000);
        client
            .apply_message_defaults(&mut over)
            .expect("an over-ceiling request is clamped, not rejected");
        assert_eq!(over.inner.max_tokens, 64_000);

        // A value at or below the ceiling is left alone.
        let mut under = messages_request(4_096);
        client
            .apply_message_defaults(&mut under)
            .expect("an under-ceiling request is untouched");
        assert_eq!(under.inner.max_tokens, 4_096);
    }

    /// Regression: with no declared ceiling the sampler used to guess 128K,
    /// which is only valid for top-tier models. Fail early with an actionable
    /// diagnostic instead of shipping a guess the provider rejects.
    #[test]
    fn messages_defaults_reject_absent_ceiling_instead_of_guessing() {
        let client = messages_client(None);

        let mut request = messages_request(0);
        let err = client
            .apply_message_defaults(&mut request)
            .expect_err("a missing ceiling must not be papered over with a guess");

        assert!(
            matches!(err, SamplingError::InvalidConfiguration(_)),
            "expected a configuration error, got: {err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("max_completion_tokens"),
            "diagnostic must name the field to set: {rendered}"
        );
        assert_eq!(
            request.inner.max_tokens, 0,
            "no max_tokens may be invented on the failure path"
        );
    }

    /// An explicit `max_tokens` stays valid without a declared ceiling: the
    /// caller has taken responsibility for the value.
    #[test]
    fn messages_defaults_keep_explicit_max_tokens_without_declared_ceiling() {
        let client = messages_client(None);

        let mut request = messages_request(8_192);
        client
            .apply_message_defaults(&mut request)
            .expect("an explicit max_tokens needs no ceiling to fall back on");
        assert_eq!(request.inner.max_tokens, 8_192);
    }

    // Regression: a past change dropped User-Agent from sampling requests.
    #[test]
    fn sampling_client_always_has_user_agent() {
        let client = SamplingClient::new(minimal_config()).expect("build");
        assert!(client.default_headers.contains_key(USER_AGENT));
    }

    // Regression: a past change dropped HeaderInjector (traceparent) from sampling requests.
    #[test]
    fn header_injector_is_called_in_post() {
        #[derive(Debug)]
        struct TestInjector;
        impl crate::config::HeaderInjector for TestInjector {
            fn inject(&self, headers: &mut HeaderMap) {
                headers.insert(
                    HeaderName::from_static("traceparent"),
                    HeaderValue::from_static("00-test-trace-id-00"),
                );
            }
        }

        let mut config = minimal_config();
        config.header_injector = Some(std::sync::Arc::new(TestInjector));
        let client = SamplingClient::new(config).expect("build");
        let SentRequest { builder, .. } = client.post("http://localhost/test");
        let req = builder.build().expect("build request");
        assert!(
            req.headers().contains_key("traceparent"),
            "HeaderInjector should inject traceparent into post() requests"
        );
    }

    #[test]
    fn user_agent_includes_origin_and_agent_product() {
        let origin = OriginClientInfo {
            product: "my-client".to_string(),
            version: Some("1.2.3".to_string()),
        };
        let ua = user_agent_string_for(&origin);
        assert!(ua.contains("my-client/1.2.3"));
        assert!(ua.contains(AGENT_PRODUCT));
    }

    #[test]
    fn user_agent_omits_origin_version_when_absent() {
        let origin = OriginClientInfo {
            product: "my-client".to_string(),
            version: None,
        };
        let ua = user_agent_string_for(&origin);
        // No slash between product and the grok-shell agent product.
        assert!(ua.starts_with("my-client grok-shell/"));
    }

    #[test]
    fn user_agent_collapses_when_origin_matches_agent() {
        let agent_version = xai_grok_version::VERSION.to_string();
        let origin = OriginClientInfo {
            product: AGENT_PRODUCT.to_string(),
            version: Some(agent_version.clone()),
        };
        let ua = user_agent_string_for(&origin);
        // Single product/version slot when the origin and agent match.
        assert!(ua.starts_with(&format!("{}/{}", AGENT_PRODUCT, agent_version)));
    }

    /// Counts callbacks for assertions in the tests below.
    #[derive(Default, Debug)]
    struct CountingCallback {
        invocations: std::sync::Mutex<Vec<(crate::attribution::SamplingConsumer, Option<String>)>>,
    }

    #[derive(Debug)]
    struct StaticBearerResolver(&'static str);

    impl crate::config::BearerResolver for StaticBearerResolver {
        fn current_bearer(&self) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    impl crate::attribution::Auth401AttributionCallback for CountingCallback {
        fn record_401(
            &self,
            consumer: crate::attribution::SamplingConsumer,
            sent_bearer: Option<&str>,
        ) {
            self.invocations
                .lock()
                .unwrap()
                .push((consumer, sent_bearer.map(|s| s.to_string())));
        }
    }

    /// `post()` strips the `"Bearer "` scheme prefix off `Authorization`
    /// and captures the tail fragment (see `BEARER_SUFFIX_LEN`).
    #[test]
    fn post_captures_bearer_tail_for_openai_compat() {
        let cfg = SamplerConfig {
            api_key: Some("test-bearer-1234567890".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/chat/completions");
        assert_eq!(bearer.as_deref(), Some("r-1234567890"));
        assert_eq!(
            bearer.as_deref().map(str::len),
            Some(crate::attribution::BEARER_SUFFIX_LEN),
        );
    }

    /// `post()` captures `x-api-key` for Messages-API backends and keeps
    /// the value's tail fragment.
    #[test]
    fn post_captures_x_api_key_tail_for_messages() {
        let cfg = SamplerConfig {
            api_key: Some("anthropic-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/messages");
        assert_eq!(bearer.as_deref(), Some("c-key-abc123"));
        assert_eq!(
            bearer.as_deref().map(str::len),
            Some(crate::attribution::BEARER_SUFFIX_LEN),
        );
    }

    /// `post()` captures `None` when the request carries no auth header.
    #[test]
    fn post_captures_none_when_no_header() {
        let cfg = SamplerConfig {
            api_key: None,
            api_backend: ApiBackend::ChatCompletions,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/chat/completions");
        assert!(bearer.is_none());
    }

    /// The race this design closes: a 401 triggers a recovery that rotates
    /// the resolver, so a record-time re-read attributes a bearer the
    /// rejected request never carried. The attributed fragment must be the
    /// one captured when the request was built.
    #[test]
    fn post_capture_is_immune_to_resolver_rotation_after_build() {
        #[derive(Debug)]
        struct RotatingResolver(std::sync::Mutex<String>);
        impl crate::config::BearerResolver for RotatingResolver {
            fn current_bearer(&self) -> Option<String> {
                Some(self.0.lock().unwrap().clone())
            }
        }

        let resolver = std::sync::Arc::new(RotatingResolver(std::sync::Mutex::new(
            "rejected-token-oldtail1".to_string(),
        )));
        let cfg = SamplerConfig {
            api_key: None,
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(resolver.clone()),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");

        let SentRequest {
            sent_bearer: sent_at_build,
            ..
        } = client.post("https://example.test/v1/responses");
        // The 401 kicks recovery; the resolver rotates before the callback runs.
        *resolver.0.lock().unwrap() = "fresh-token-newtail99".to_string();

        assert_eq!(
            sent_at_build.as_deref(),
            Some("ken-oldtail1"),
            "attribution must describe the bearer the rejected request carried"
        );
        // A record-time re-read (the pre-fix behavior) would report the
        // rotated token instead:
        assert_eq!(
            client.current_sent_bearer_suffix().as_deref(),
            Some("en-newtail99"),
            "sanity: the build-time capture and a live re-read now differ"
        );
    }

    #[test]
    fn live_bearer_resolver_uses_authorization_for_messages_plus_bearer() {
        let cfg = SamplerConfig {
            api_key: Some("stale-bearer".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::Bearer,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-bearer"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/messages");
        let request = builder.build().expect("request should build");
        let auth = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        assert_eq!(auth, Some("Bearer fresh-bearer"));
        assert!(request.headers().get("x-api-key").is_none());
    }

    /// Regression: when `api_key` (which seeds `default_headers` with an
    /// `Authorization: Bearer ...`) AND a `bearer_resolver` are both set,
    /// `post()` must produce **exactly one** `Authorization` header on the
    /// wire. The pre-fix code used `RequestBuilder::header(AUTHORIZATION, ...)`
    /// which appends rather than replaces, causing two identical
    /// `Authorization` headers and a 400 from cli-chat-proxy.
    #[test]
    fn post_emits_single_authorization_with_api_key_and_bearer_resolver() {
        let cfg = SamplerConfig {
            api_key: Some("stale-bearer".to_string()),
            api_backend: ApiBackend::Responses,
            auth_scheme: AuthScheme::Bearer,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-bearer"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/responses");
        let request = builder.build().expect("request should build");
        let auth_count = request.headers().get_all(AUTHORIZATION).iter().count();
        assert_eq!(
            auth_count, 1,
            "expected exactly one Authorization header, got {auth_count}"
        );
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer fresh-bearer"),
        );
    }

    #[test]
    fn live_bearer_resolver_uses_x_api_key_for_messages_plus_anthropic_api_key() {
        let cfg = SamplerConfig {
            api_key: Some("stale-anthropic".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-anthropic"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/messages");
        let request = builder.build().expect("request should build");
        let api_key = request
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok());
        assert_eq!(api_key, Some("fresh-anthropic"));
        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    /// The callback receives the `post()`-captured fragment only — the
    /// full bearer never crosses the crate boundary.
    #[test]
    fn record_401_attribution_invokes_callback_with_captured_bearer() {
        let cb = std::sync::Arc::new(CountingCallback::default());
        let cb_dyn: crate::attribution::SharedAttributionCallback = cb.clone();
        let cfg = SamplerConfig {
            api_key: Some("the-bearer-1234567890-extra-tail".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            attribution_callback: Some(cb_dyn),
            bearer_resolver: None,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { sent_bearer, .. } =
            client.post("https://example.test/v1/chat/completions");
        client.record_401_attribution(
            crate::attribution::SamplingConsumer::ChatCompletionsStream,
            sent_bearer.as_deref(),
        );
        let calls = cb.invocations.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            crate::attribution::SamplingConsumer::ChatCompletionsStream
        );
        assert_eq!(calls[0].1.as_deref(), Some("0-extra-tail"));
        assert_eq!(
            calls[0].1.as_deref().map(str::len),
            Some(crate::attribution::BEARER_SUFFIX_LEN),
        );
    }

    /// When a bearer_resolver is wired but returns `None`, attribution must
    /// report no sent bearer (not the construction-time default header seed).
    #[test]
    fn bearer_resolver_none_attribution_ignores_default_headers() {
        #[derive(Debug)]
        struct EmptyResolver;
        impl crate::config::BearerResolver for EmptyResolver {
            fn current_bearer(&self) -> Option<String> {
                None
            }
        }

        let cfg = SamplerConfig {
            api_key: Some("stale-seed-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(std::sync::Arc::new(EmptyResolver)),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert_eq!(
            client.current_sent_bearer_suffix(),
            None,
            "resolver None must not attribute a stripped default seed"
        );
    }

    /// When a bearer_resolver is wired but returns `None` (hard-expired
    /// session with no live AT), default Authorization / x-api-key must be
    /// stripped so a stale seed key cannot ride the wire.
    #[test]
    fn bearer_resolver_none_strips_default_authorization() {
        #[derive(Debug)]
        struct EmptyResolver;
        impl crate::config::BearerResolver for EmptyResolver {
            fn current_bearer(&self) -> Option<String> {
                None
            }
        }

        let cfg = SamplerConfig {
            api_key: Some("stale-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(std::sync::Arc::new(EmptyResolver)),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            builder,
            sent_bearer: sent,
        } = client.post("https://example.test/v1/responses");
        let request = builder.body("").build().expect("request should build");
        assert_eq!(sent, None, "capture must agree: nothing was sent");
        assert!(
            request.headers().get(AUTHORIZATION).is_none(),
            "stale default Authorization must not be sent when resolver is empty"
        );
    }

    /// Regression test: when a bearer_resolver is wired, `post()` must
    /// *replace* the Authorization header from `default_headers`, not
    /// append a second one. Duplicate Authorization headers cause
    /// Cloudflare to return 400 Bad Request.
    #[test]
    fn bearer_resolver_replaces_authorization_header() {
        #[derive(Debug)]
        struct StaticResolver(String);
        impl crate::config::BearerResolver for StaticResolver {
            fn current_bearer(&self) -> Option<String> {
                Some(self.0.clone())
            }
        }

        let resolver: crate::config::SharedBearerResolver =
            std::sync::Arc::new(StaticResolver("fresh-token".to_string()));
        let cfg = SamplerConfig {
            api_key: Some("stale-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(resolver),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");

        // Build a request to inspect the final headers.
        let SentRequest { builder, .. } = client.post("https://example.test/v1/responses");
        let request = builder.body("").build().expect("request should build");

        let auth_values: Vec<_> = request.headers().get_all(AUTHORIZATION).iter().collect();
        assert_eq!(
            auth_values.len(),
            1,
            "expected exactly one Authorization header, got {}: {:?}",
            auth_values.len(),
            auth_values
        );
        assert_eq!(
            auth_values[0].to_str().unwrap(),
            "Bearer fresh-token",
            "Authorization header should contain the resolver's fresh token"
        );
    }

    /// `record_401_attribution` is a no-op when `attribution_callback`
    /// is `None` (the BYOK / sampler-only path). The previous tests
    /// in this module construct clients without a callback and rely
    /// on this property holding.
    #[test]
    fn record_401_attribution_is_noop_without_callback() {
        let cfg = SamplerConfig {
            api_key: Some("bearer".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            attribution_callback: None,
            bearer_resolver: None,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        // Must not panic.
        client.record_401_attribution(
            crate::attribution::SamplingConsumer::ChatCompletions,
            Some("bearer-tail-12"),
        );
    }

    /// `response.completed` carrying
    /// `usage.context_details.{input_tokens, output_tokens}` rewrites
    /// `usage.total_tokens` in place to the live context length
    /// (`ctx.input + ctx.output`). Billing fields stay on the wire's
    /// cumulative values.
    #[test]
    fn deserialize_response_event_overrides_total_tokens_from_context_details() {
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "grok-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 6003,
                    "input_tokens_details": { "cached_tokens": 1984 },
                    "output_tokens": 711,
                    "output_tokens_details": { "reasoning_tokens": 388 },
                    "total_tokens": 6714,
                    "context_details": {
                        "input_tokens": 5022,
                        "output_tokens": 571
                    }
                }
            }
        }"#;
        let event = deserialize_response_event(sse)
            .expect("parse")
            .expect("a consumed event type decodes");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        // Billing fields stay cumulative — unchanged by context_details.
        assert_eq!(usage.input_tokens, 6003);
        assert_eq!(usage.output_tokens, 711);
        assert_eq!(usage.input_tokens_details.cached_tokens, 1984);
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 388);
        // total_tokens rewritten to ctx.input + ctx.output (5022 + 571).
        // NOT the wire's cumulative total (6714).
        assert_eq!(usage.total_tokens, 5_593);
    }

    #[test]
    fn deserialize_response_event_stashes_cost_in_metadata() {
        let make = |ticks: i64| {
            format!(
                r#"{{
                "type": "response.completed",
                "sequence_number": 0,
                "response": {{
                    "id": "resp_1", "object": "response", "created_at": 0,
                    "model": "grok-build", "status": "completed", "output": [],
                    "usage": {{
                        "input_tokens": 10,
                        "input_tokens_details": {{ "cached_tokens": 0 }},
                        "output_tokens": 5,
                        "output_tokens_details": {{ "reasoning_tokens": 0 }},
                        "total_tokens": 15,
                        "cost_in_usd_ticks": {ticks}
                    }}
                }}
            }}"#
            )
        };

        let event = deserialize_response_event(&make(78))
            .expect("parse")
            .expect("a consumed event type decodes");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        assert_eq!(
            e.response
                .metadata
                .as_ref()
                .and_then(|m| m.get(COST_USD_TICKS_METADATA_KEY))
                .map(String::as_str),
            Some("78")
        );

        // The REST mapper backfills 0 for unbilled requests: no stash.
        let event = deserialize_response_event(&make(0))
            .expect("parse")
            .expect("a consumed event type decodes");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        assert!(e.response.metadata.is_none());
    }

    #[test]
    fn deserialize_response_event_total_tokens_unchanged_when_context_details_absent() {
        // Older / non-Responses backends omit `context_details`.
        // `total_tokens` passes through from the wire unchanged.
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "grok-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 10000,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens": 100,
                    "output_tokens_details": { "reasoning_tokens": 0 },
                    "total_tokens": 10100
                }
            }
        }"#;
        let event = deserialize_response_event(sse)
            .expect("parse")
            .expect("a consumed event type decodes");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        assert_eq!(usage.total_tokens, 10_100);
    }

    #[test]
    fn deserialize_response_event_total_tokens_unchanged_when_context_details_partial() {
        // Defensive: if the backend ever ships only one of the two
        // context_details fields, we don't have a complete picture of
        // the live context size, so leave `total_tokens` on the wire's
        // cumulative value instead of guessing (treating the missing
        // half as 0 would silently under-report).
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "grok-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 6003,
                    "input_tokens_details": { "cached_tokens": 1984 },
                    "output_tokens": 711,
                    "output_tokens_details": { "reasoning_tokens": 388 },
                    "total_tokens": 6714,
                    "context_details": {
                        "input_tokens": 5022
                    }
                }
            }
        }"#;
        let event = deserialize_response_event(sse)
            .expect("parse")
            .expect("a consumed event type decodes");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        assert_eq!(usage.total_tokens, 6_714);
    }

    #[test]
    fn deserialize_response_event_ignores_context_details_on_non_terminal_events() {
        // Non-terminal events don't carry final usage; even if the backend ever
        // echoed `context_details` on one, we don't touch it.
        let sse = r#"{
            "type": "response.output_text.delta",
            "sequence_number": 0,
            "item_id": "item-1",
            "output_index": 0,
            "content_index": 0,
            "delta": "hello",
            "logprobs": []
        }"#;
        let event = deserialize_response_event(sse)
            .expect("non-terminal event parses")
            .expect("a consumed event type decodes");
        assert!(matches!(
            event,
            rs::ResponseStreamEvent::ResponseOutputTextDelta(_)
        ));
    }

    /// A gateway heartbeat (`keepalive`) is not in the pinned
    /// `rs::ResponseStreamEvent` and cannot be added to it here. Nothing
    /// downstream reads it, so it is skipped rather than failing the turn —
    /// the failure was deterministic, so the retry died on it too.
    #[test]
    fn deserialize_response_event_skips_unknown_event_type() {
        for sse in [
            r#"{"type": "keepalive"}"#,
            r#"{"type": "response.some_future_thing.delta", "delta": "x"}"#,
            // No `type` at all: not an event this pipeline reads either.
            r#"{"keepalive": true}"#,
        ] {
            let event =
                deserialize_response_event(sse).expect("an unrecognized event must not error");
            assert!(event.is_none(), "expected a skipped frame for {sse}");
        }
    }

    /// The counterpart: a type the stream transform *does* read is real data,
    /// so a malformed frame of it stays an error instead of being dropped.
    #[test]
    fn deserialize_response_event_errors_on_malformed_known_event() {
        // `delta` must be a string, and the rest of the required fields are
        // missing — a consumed type that cannot be decoded.
        let sse = r#"{"type": "response.output_text.delta", "delta": 7}"#;
        assert!(matches!(
            deserialize_response_event(sse),
            Err(SamplingError::Serialization(_))
        ));

        // Terminal frames matter most: dropping one loses the response.
        let sse = r#"{"type": "response.completed", "sequence_number": 0}"#;
        assert!(matches!(
            deserialize_response_event(sse),
            Err(SamplingError::Serialization(_))
        ));
    }
}
