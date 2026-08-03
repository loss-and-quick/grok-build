//! Distillation: answer the caller's question *about* a fetched page instead of
//! handing back the first N kilobytes of it.
//!
//! Today an oversized page is truncated — the caller gets the head of the
//! document and the tail is only reachable through the persisted artifact. This
//! module adds a second, optional step: run the converted markdown past an
//! auxiliary model together with the caller's `prompt`, and return that answer
//! as the tool body.
//!
//! # Fail open, always
//!
//! Distillation can only ever *replace* a body that was already produced. Every
//! path that cannot produce an answer — distillation switched off, no distiller
//! registered, no prompt, an unresolvable model pin, a timeout, a transport
//! error, an empty completion — returns the raw output untouched. Returning
//! nothing because a helper model was unavailable would be strictly worse than
//! the truncation it replaces, so it is not a reachable state.
//!
//! # Why a catalog key, and not a wire model
//!
//! A configured `[toolset.web_fetch.distill] model` value is a catalog *key*,
//! and a catalog key is not a routing slug: a `[[provider]]` block is expanded
//! into `<provider>/<model>` keys whose slug is the bare `<model>`, so the key
//! must never be forwarded as a request's `model`. But it must not be *reduced*
//! to that slug here either. The qualified key is the only thing that names one
//! entry when two providers serve the same slug, so translating it early throws
//! away the disambiguation and lets the host re-resolve to the other provider —
//! its endpoint, its credential — after the operator spelled one.
//!
//! So [`WebContentDistiller`] splits "is this pin in the catalog" from "run an
//! inference on it": this module hands [`WebContentDistiller::infer`] the key
//! [`WebContentDistiller::catalog_key`] gave back, and the host resolves that
//! key exactly once into an endpoint, a credential and the entry's own routing
//! slug. An id the catalog cannot place skips distillation rather than guessing.

use std::sync::Arc;

use super::config::WebFetchParams;
use crate::register_resource;
use crate::types::output::WebFetchOutput;
use crate::util::truncate::truncate_str;

/// Appended to the page text when it had to be clamped for the helper model.
const INPUT_TRUNCATION_MARKER: &str = "\n\n[Content truncated due to length...]";

/// Instruction given to the auxiliary model. Deliberately terse: the page and
/// the caller's question carry the task, and a long preamble only competes with
/// them for the helper model's attention.
pub const DISTILL_SYSTEM_PROMPT: &str = concat!(
    "You read a single web page and answer one question about it for a coding ",
    "agent. Answer from the page only. If the page does not contain the answer, ",
    "say so plainly rather than guessing."
);

/// A backend that can run a single-turn auxiliary completion for `web_fetch`.
///
/// Defined here so `xai-grok-tools` stays inference-backend-agnostic; the
/// concrete implementation lives in the shell, which owns the model catalog and
/// the sampler, and is injected as `Arc<dyn WebContentDistiller>` through
/// `Resources` (same shape as `MemoryBackend` for `memory_search`).
///
/// Implementors must honour three rules that this crate cannot enforce:
///
/// 1. [`Self::infer`] receives a catalog key and must resolve it exactly once,
///    putting the resolved entry's own routing slug on the wire. It must not
///    forward the key as the request's `model`, and it must not re-resolve a
///    slug it derived along the way — the key is what names the entry.
/// 2. The call must not ride the session's client under a foreign slug. An
///    auxiliary model belongs to whichever provider the catalog entry names, and
///    that provider's own endpoint and credential are the ones to use.
/// 3. A session talking to a custom provider must not have a first-party
///    request fabricated on its behalf. When no auxiliary model is reachable for
///    the session as configured, return `None` from
///    [`Self::default_catalog_key`] and let distillation be skipped.
#[async_trait::async_trait]
pub trait WebContentDistiller: Send + Sync {
    /// Place a configured model pin in the catalog, answering with the entry's
    /// catalog *key* — the spelling that names one entry rather than a family of
    /// same-slug ones. Returns `None` when the catalog does not list the id.
    fn catalog_key(&self, model_pin: &str) -> Option<String>;

    /// Catalog key to use when nothing is pinned, or `None` when no auxiliary
    /// model is available for this session.
    fn default_catalog_key(&self) -> Option<String>;

    /// Run a single-turn, tool-free completion on the entry `model_key` names.
    async fn infer(
        &self,
        model_key: &str,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

/// Resource wrapper injected into a session's `Resources` by the host.
///
/// Wraps an `Arc<dyn WebContentDistiller>` so one backend can be shared across
/// concurrent `web_fetch` calls in the same session. Absent from `Resources`
/// means "no distillation available", which is a supported state, not an error —
/// see the fail-open note on this module.
#[derive(Clone)]
pub struct WebContentDistillerResource(pub Arc<dyn WebContentDistiller>);

impl WebContentDistillerResource {
    pub fn distiller(&self) -> &dyn WebContentDistiller {
        self.0.as_ref()
    }
}

impl std::fmt::Debug for WebContentDistillerResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebContentDistillerResource").finish()
    }
}

register_resource!(
    "grok_build",
    "WebContentDistillerResource",
    WebContentDistillerResource
);

/// Everything the distillation step needs that is not already on the output.
pub(super) struct DistillContext<'a> {
    pub(super) params: &'a WebFetchParams,
    pub(super) distiller: Option<&'a dyn WebContentDistiller>,
    /// What the caller wants to know about the page. `None` or blank means the
    /// caller asked for the page itself, and no helper model is spent.
    pub(super) prompt: Option<&'a str>,
    /// Whether the fetched host is on the built-in preapproved list.
    pub(super) preapproved_source: bool,
    /// Name of the read tool, for the "the full page is still on disk" pointer.
    pub(super) read_tool_name: Option<&'a str>,
}

/// Replace the body of `output` with an answer to `ctx.prompt`, or return
/// `output` exactly as received.
pub(super) async fn apply(output: WebFetchOutput, ctx: DistillContext<'_>) -> WebFetchOutput {
    let WebFetchOutput::Content(ref content) = output else {
        return output;
    };
    if !ctx.params.distill() {
        return output;
    }
    let Some(prompt) = ctx.prompt.map(str::trim).filter(|p| !p.is_empty()) else {
        return output;
    };
    let Some(distiller) = ctx.distiller else {
        tracing::debug!("web_fetch distillation requested but no distiller is registered");
        return output;
    };
    // A preapproved page that arrived whole needs no helper model: the caller
    // already has every byte, so a summary could only lose information.
    if ctx.preapproved_source && content.source_artifact.is_none() {
        tracing::debug!("web_fetch skipping distillation: preapproved page fits inline");
        return output;
    }

    let Some(model_key) = resolve_model_key(distiller, ctx.params.distill_model()) else {
        return output;
    };

    let user_prompt = build_user_prompt(
        &content.content,
        prompt,
        ctx.preapproved_source,
        ctx.params.distill_max_input_bytes(),
    );

    let answer = match tokio::time::timeout(
        ctx.params.distill_timeout(),
        distiller.infer(&model_key, DISTILL_SYSTEM_PROMPT, &user_prompt),
    )
    .await
    {
        Ok(Ok(answer)) => answer,
        Ok(Err(e)) => {
            tracing::warn!("web_fetch distillation failed, returning raw page: {e}");
            return output;
        }
        Err(_) => {
            tracing::warn!(
                "web_fetch distillation timed out after {:?}, returning raw page",
                ctx.params.distill_timeout()
            );
            return output;
        }
    };
    if answer.trim().is_empty() {
        tracing::warn!("web_fetch distillation returned an empty answer, returning raw page");
        return output;
    }

    let WebFetchOutput::Content(mut content) = output else {
        unreachable!("matched Content above");
    };
    let footer = recovery_footer(&content, ctx.read_tool_name);
    content.content = format!("{}{footer}", answer.trim_end());
    WebFetchOutput::Content(content)
}

/// The catalog key to distill with, or `None` to skip distillation.
///
/// A pin is *placed*, never translated: `catalog_key` guards (an id the catalog
/// does not list is not routable) and normalises the spelling to the one that
/// names a single entry. Deriving a wire slug is the host's job, downstream of
/// picking the endpoint, so that the two cannot come from different entries.
fn resolve_model_key(distiller: &dyn WebContentDistiller, pin: Option<&str>) -> Option<String> {
    match pin {
        Some(pin) => match distiller.catalog_key(pin) {
            Some(key) => Some(key),
            None => {
                tracing::warn!(
                    "web_fetch distill_model {pin:?} is not in the model catalog; \
                     returning the raw page instead of guessing a route for it"
                );
                None
            }
        },
        None => {
            let key = distiller.default_catalog_key();
            if key.is_none() {
                tracing::debug!(
                    "web_fetch skipping distillation: no auxiliary model is available \
                     for this session"
                );
            }
            key
        }
    }
}

/// Note that the answer is a distillation and where the untouched page went.
///
/// Replacing the body drops the truncation footer the overflow handler wrote, so
/// the pointer to the persisted artifact has to be re-stated — otherwise
/// distillation would quietly cost the caller its recovery path.
fn recovery_footer(
    content: &crate::types::output::WebFetchContent,
    read_tool_name: Option<&str>,
) -> String {
    let Some(artifact) = &content.source_artifact else {
        return String::new();
    };
    let steer = match read_tool_name {
        Some(tool) => format!(" Use `{tool}` on that file for anything this answer omits."),
        None => String::new(),
    };
    format!(
        "\n\n[web_fetch: distilled from the fetched page ({} bytes). \
         Complete page saved to: {}.{steer}]",
        content.bytes,
        artifact.path.display()
    )
}

/// Render the page plus the caller's question for the auxiliary model.
///
/// `max_input_bytes` clamps the page so a large document cannot overflow the
/// helper model's own context — a rejected oversized request would fail open
/// into the truncation this feature exists to avoid.
pub(super) fn build_user_prompt(
    markdown: &str,
    prompt: &str,
    preapproved_source: bool,
    max_input_bytes: usize,
) -> String {
    let clamped = truncate_str(markdown, max_input_bytes);
    let marker = if clamped.len() < markdown.len() {
        INPUT_TRUNCATION_MARKER
    } else {
        ""
    };
    let guidelines = if preapproved_source {
        PREAPPROVED_GUIDELINES
    } else {
        UNVETTED_GUIDELINES
    };
    format!("Web page content:\n---\n{clamped}{marker}\n---\n\n{prompt}\n\n{guidelines}\n")
}

/// Guidance for a page from the built-in preapproved list — documentation the
/// tool is expected to quote from freely.
const PREAPPROVED_GUIDELINES: &str = "\
Answer from the content above. Include the relevant details, code examples and \
documentation excerpts the question calls for.";

/// Guidance for any other host. The tool cannot vouch for what it fetched, so
/// the answer stays short on verbatim reproduction.
const UNVETTED_GUIDELINES: &str = "\
Answer using only the content above. In your answer:
 - Keep any quotation from the page under 125 characters. Open source material \
may be quoted at the length its licence allows.
 - Put exact wording from the page in quotation marks; anything outside \
quotation marks must be your own phrasing, not a word-for-word copy.
 - Do not comment on the legality of this request or of your own answer.
 - Never reproduce song lyrics.";

#[cfg(test)]
mod tests {
    use super::super::config::DistillParams;
    use super::*;
    use crate::types::output::{WebFetchContent, WebFetchSourceArtifact};
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Records what it was asked to run, so a test can assert on the model id
    /// that reached the backend.
    struct FakeDistiller {
        /// `model_pin -> catalog key`. A pin absent from the map is "not in the
        /// catalog" and must not reach `infer`.
        catalog: Vec<(String, String)>,
        default_key: Option<String>,
        answer: Result<String, String>,
        delay: Option<std::time::Duration>,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl FakeDistiller {
        fn answering(answer: &str) -> Self {
            Self {
                catalog: Vec::new(),
                default_key: Some("default-key".into()),
                answer: Ok(answer.into()),
                delay: None,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn failing() -> Self {
            Self {
                answer: Err("upstream is down".into()),
                ..Self::answering("")
            }
        }

        fn with_catalog(mut self, entries: &[(&str, &str)]) -> Self {
            self.catalog = entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            self
        }

        fn with_default_key(mut self, slug: Option<&str>) -> Self {
            self.default_key = slug.map(str::to_owned);
            self
        }

        fn with_delay(mut self, delay: std::time::Duration) -> Self {
            self.delay = Some(delay);
            self
        }

        /// Model ids this distiller was actually asked to infer with.
        fn inferred_models(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(model, _)| model.clone())
                .collect()
        }

        fn last_user_prompt(&self) -> Option<String> {
            self.calls.lock().unwrap().last().map(|(_, p)| p.clone())
        }
    }

    #[async_trait::async_trait]
    impl WebContentDistiller for FakeDistiller {
        fn catalog_key(&self, model_pin: &str) -> Option<String> {
            self.catalog
                .iter()
                .find(|(pin, _)| pin == model_pin)
                .map(|(_, key)| key.clone())
        }

        fn default_catalog_key(&self) -> Option<String> {
            self.default_key.clone()
        }

        async fn infer(
            &self,
            model_key: &str,
            _system_prompt: &str,
            user_prompt: &str,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.calls
                .lock()
                .unwrap()
                .push((model_key.to_owned(), user_prompt.to_owned()));
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            self.answer.clone().map_err(Into::into)
        }
    }

    const RAW_PAGE: &str = "# Heading\n\nraw markdown body";

    fn distill_params(distill: DistillParams) -> WebFetchParams {
        WebFetchParams {
            distill: Some(Box::new(distill)),
            ..WebFetchParams::default()
        }
    }

    fn content(source_artifact: Option<&str>) -> WebFetchOutput {
        WebFetchOutput::Content(WebFetchContent {
            url: "https://example.com/".into(),
            content: RAW_PAGE.into(),
            content_type: "markdown".into(),
            status_code: 200,
            bytes: RAW_PAGE.len(),
            source_artifact: source_artifact.map(|p| WebFetchSourceArtifact {
                path: PathBuf::from(p),
            }),
            inline_fallback: None,
            output_location: None,
        })
    }

    fn body(output: &WebFetchOutput) -> &str {
        match output {
            WebFetchOutput::Content(c) => &c.content,
            other => panic!("expected Content, got {other:?}"),
        }
    }

    fn ctx<'a>(
        params: &'a WebFetchParams,
        distiller: &'a FakeDistiller,
        prompt: Option<&'a str>,
    ) -> DistillContext<'a> {
        DistillContext {
            params,
            distiller: Some(distiller),
            prompt,
            preapproved_source: false,
            read_tool_name: None,
        }
    }

    #[tokio::test]
    async fn successful_distillation_replaces_the_body() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("the rate limit is 60 rpm");
        let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
        assert_eq!(body(&out), "the rate limit is 60 rpm");
    }

    #[tokio::test]
    async fn distiller_error_returns_the_raw_markdown_unchanged() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::failing();
        let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
        assert_eq!(body(&out), RAW_PAGE);
    }

    #[tokio::test]
    async fn distiller_timeout_returns_the_raw_markdown_unchanged() {
        let params = distill_params(DistillParams {
            timeout_secs: Some(1),
            ..DistillParams::default()
        });
        let distiller = FakeDistiller::answering("never arrives")
            .with_delay(std::time::Duration::from_secs(30));
        tokio::time::pause();
        let fetch = apply(content(None), ctx(&params, &distiller, Some("rate limit?")));
        let out = tokio::time::timeout(std::time::Duration::from_secs(120), fetch)
            .await
            .expect("the distillation timeout must fire before the test's");
        assert_eq!(body(&out), RAW_PAGE);
    }

    #[tokio::test]
    async fn empty_answer_returns_the_raw_markdown_unchanged() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("  \n ");
        let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
        assert_eq!(body(&out), RAW_PAGE);
    }

    #[tokio::test]
    async fn no_prompt_means_no_model_call() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("should not be reached");
        let out = apply(content(None), ctx(&params, &distiller, None)).await;
        assert_eq!(body(&out), RAW_PAGE);
        assert!(
            distiller.inferred_models().is_empty(),
            "no prompt must not spend a call"
        );

        let out = apply(content(None), ctx(&params, &distiller, Some("   "))).await;
        assert_eq!(body(&out), RAW_PAGE);
        assert!(
            distiller.inferred_models().is_empty(),
            "a blank prompt is not a prompt"
        );
    }

    #[tokio::test]
    async fn disabled_by_config_means_no_model_call() {
        let params = distill_params(DistillParams {
            enabled: Some(false),
            ..DistillParams::default()
        });
        let distiller = FakeDistiller::answering("should not be reached");
        let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
        assert_eq!(body(&out), RAW_PAGE);
        assert!(distiller.inferred_models().is_empty());
    }

    #[tokio::test]
    async fn no_distiller_registered_returns_the_raw_markdown_unchanged() {
        let params = WebFetchParams::default();
        let out = apply(
            content(None),
            DistillContext {
                params: &params,
                distiller: None,
                prompt: Some("rate limit?"),
                preapproved_source: false,
                read_tool_name: None,
            },
        )
        .await;
        assert_eq!(body(&out), RAW_PAGE);
    }

    /// The backend is handed the catalog key, whichever spelling the operator
    /// used. A bare slug is widened to the key it names; a qualified pin is
    /// passed through as itself rather than narrowed to its slug, because that
    /// qualification is the only thing distinguishing two providers that serve
    /// one slug.
    #[tokio::test]
    async fn a_configured_pin_reaches_the_backend_as_its_catalog_key() {
        for spelling in ["acme/some-model", "some-model"] {
            let params = distill_params(DistillParams {
                model: Some(spelling.into()),
                ..DistillParams::default()
            });
            let distiller = FakeDistiller::answering("distilled").with_catalog(&[
                ("acme/some-model", "acme/some-model"),
                ("some-model", "acme/some-model"),
            ]);
            let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
            assert_eq!(body(&out), "distilled");
            assert_eq!(
                distiller.inferred_models(),
                vec!["acme/some-model".to_string()],
                "pin {spelling:?} must reach the backend as the catalog key"
            );
        }
    }

    #[tokio::test]
    async fn a_pin_the_catalog_cannot_place_skips_distillation() {
        let params = distill_params(DistillParams {
            model: Some("unknown-model".into()),
            ..DistillParams::default()
        });
        let distiller = FakeDistiller::answering("should not be reached");
        let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
        assert_eq!(body(&out), RAW_PAGE);
        assert!(
            distiller.inferred_models().is_empty(),
            "a pin with no catalog entry must not be guessed at"
        );
    }

    #[tokio::test]
    async fn unpinned_distillation_uses_the_backend_default_key() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("distilled");
        let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
        assert_eq!(body(&out), "distilled");
        assert_eq!(distiller.inferred_models(), vec!["default-key".to_string()]);
    }

    /// No auxiliary model is reachable for this session — a custom-provider
    /// session with nothing routable, say. Skip, rather than have a first-party
    /// request fabricated on its behalf.
    #[tokio::test]
    async fn no_default_key_available_skips_distillation() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("should not be reached").with_default_key(None);
        let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
        assert_eq!(body(&out), RAW_PAGE);
        assert!(distiller.inferred_models().is_empty());
    }

    #[tokio::test]
    async fn a_preapproved_page_that_fits_inline_is_not_distilled() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("should not be reached");
        let out = apply(
            content(None),
            DistillContext {
                preapproved_source: true,
                ..ctx(&params, &distiller, Some("rate limit?"))
            },
        )
        .await;
        assert_eq!(body(&out), RAW_PAGE);
        assert!(distiller.inferred_models().is_empty());
    }

    #[tokio::test]
    async fn a_preapproved_page_that_overflowed_is_still_distilled() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("distilled");
        let out = apply(
            content(Some("/sessions/a/web_fetch/1.md")),
            DistillContext {
                preapproved_source: true,
                ..ctx(&params, &distiller, Some("rate limit?"))
            },
        )
        .await;
        assert!(body(&out).starts_with("distilled"));
        assert_eq!(distiller.inferred_models(), vec!["default-key".to_string()]);
    }

    #[tokio::test]
    async fn distilled_body_keeps_the_pointer_to_the_persisted_page() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("answer");
        let out = apply(
            content(Some("/sessions/a/web_fetch/1.md")),
            DistillContext {
                read_tool_name: Some("ReadAsset"),
                ..ctx(&params, &distiller, Some("rate limit?"))
            },
        )
        .await;
        let body = body(&out);
        assert!(body.starts_with("answer"), "got: {body}");
        assert!(
            body.contains("/sessions/a/web_fetch/1.md"),
            "distillation must not cost the caller its recovery path: {body}"
        );
        assert!(body.contains("ReadAsset"), "got: {body}");
    }

    #[tokio::test]
    async fn distilled_body_has_no_footer_when_nothing_was_persisted() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("answer");
        let out = apply(content(None), ctx(&params, &distiller, Some("rate limit?"))).await;
        assert_eq!(body(&out), "answer");
    }

    #[tokio::test]
    async fn non_content_outputs_pass_through_untouched() {
        let params = WebFetchParams::default();
        let distiller = FakeDistiller::answering("should not be reached");
        let redirect = WebFetchOutput::CrossHostRedirect {
            original_host: "example.com".into(),
            redirect_url: "https://elsewhere.example.org/".into(),
        };
        let out = apply(redirect, ctx(&params, &distiller, Some("rate limit?"))).await;
        assert!(matches!(out, WebFetchOutput::CrossHostRedirect { .. }));
        assert!(distiller.inferred_models().is_empty());
    }

    #[tokio::test]
    async fn oversized_page_is_clamped_before_it_reaches_the_model() {
        let params = distill_params(DistillParams {
            max_input_bytes: Some(512),
            ..DistillParams::default()
        });
        let distiller = FakeDistiller::answering("distilled");
        let mut big = content(None);
        let huge = "Z".repeat(10_000);
        if let WebFetchOutput::Content(c) = &mut big {
            c.content = huge;
        }
        let _ = apply(big, ctx(&params, &distiller, Some("rate limit?"))).await;
        let prompt = distiller.last_user_prompt().expect("the model was called");
        assert!(prompt.contains(INPUT_TRUNCATION_MARKER), "got: {prompt}");
        assert!(
            prompt.matches('Z').count() == 512,
            "exactly the clamp's worth of page should survive"
        );
    }

    #[test]
    fn user_prompt_carries_the_page_and_the_question() {
        let rendered = build_user_prompt("PAGE BODY", "what is the rate limit?", true, 100_000);
        assert!(rendered.contains("PAGE BODY"));
        assert!(rendered.contains("what is the rate limit?"));
        assert!(rendered.contains(PREAPPROVED_GUIDELINES));
        assert!(!rendered.contains(INPUT_TRUNCATION_MARKER));
    }

    #[test]
    fn unvetted_sources_get_the_stricter_quoting_guidance() {
        let rendered = build_user_prompt("PAGE BODY", "question", false, 100_000);
        assert!(rendered.contains(UNVETTED_GUIDELINES));
        assert!(!rendered.contains(PREAPPROVED_GUIDELINES));
    }

    #[test]
    fn clamping_never_splits_a_utf8_boundary() {
        // A 4-byte character straddling the clamp must not panic or corrupt.
        let page = "\u{1F600}".repeat(64);
        let rendered = build_user_prompt(&page, "question", true, 10);
        assert!(rendered.contains(INPUT_TRUNCATION_MARKER));
        assert_eq!(rendered.matches('\u{1F600}').count(), 2);
    }
}
