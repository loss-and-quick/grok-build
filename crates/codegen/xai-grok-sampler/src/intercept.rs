//! Injectable seams for outbound request rewriting and error-driven
//! failover directives.
//!
//! Both seams follow the same callback-injection idiom as
//! [`crate::attribution`] and [`crate::config::BearerResolver`]: the
//! sampler stays free of any dependency on the shell's hook machinery.
//! The caller (the shell) wires an implementation that bridges to
//! whatever it wants -- typically the plugin hook dispatcher -- and the
//! sampler invokes it at a well-defined point.
//!
//! ## Secrets never cross the seam
//!
//! The [`RequestView`] handed to a [`RequestInterceptor`] carries the
//! request headers with the `Authorization` and `x-api-key` values
//! **removed**. A replacement may rewrite the body, the model, and the
//! non-auth headers; the sampler re-attaches credentials afterwards (via
//! the construction-time headers and the live
//! [`crate::config::BearerResolver`]). An interceptor therefore can
//! never see or forge a bearer.

use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A boxed, `Send` future returned by the async seam callbacks.
pub type SeamFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// A serializable, credential-free view of an outbound request, handed
/// to a [`RequestInterceptor`] before credentials are attached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestView {
    /// The API path being targeted (`chat/completions`, `responses`, or
    /// `messages`). Lets an interceptor scope its behavior per endpoint.
    pub endpoint: String,
    /// The model the request will be sent with (read from the body, or
    /// the client default when the body omits it).
    pub model: String,
    /// The base URL the request targets. Named an "alias" because a
    /// replacement can rewrite the model to route elsewhere; the sampler
    /// itself does not resolve base-URL aliases (that is the caller's
    /// job), so this field is informational for the interceptor.
    pub base_url_alias: String,
    /// Request headers with the `Authorization` and `x-api-key` values
    /// stripped. See the module docs.
    pub headers: Vec<(String, String)>,
    /// The full request body as JSON.
    pub body: serde_json::Value,
}

/// An optional replacement produced by a [`RequestInterceptor`]. Every
/// field is optional; `None` means "leave this part unchanged".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestReplacement {
    /// Replacement body. When present, it is sent verbatim in place of
    /// the original.
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    /// Replacement model. When present, it is written into the body's
    /// `model` field (the wire carries the model in the body).
    #[serde(default)]
    pub model: Option<String>,
    /// Replacement non-auth headers. When present, they replace the
    /// request's non-auth headers wholesale; credentials are re-attached
    /// by the sampler regardless.
    #[serde(default)]
    pub headers: Option<Vec<(String, String)>>,
}

/// Hook invoked by [`crate::SamplingClient`] just before an outbound
/// request is built, giving the caller a chance to rewrite it. Returning
/// `None` leaves the request untouched (the fail-open default).
//
// The `Debug` bound is structural: `SamplerConfig` derives `Debug` and
// carries an `Option<Arc<dyn RequestInterceptor>>`. Keep it.
pub trait RequestInterceptor: Send + Sync + std::fmt::Debug {
    /// Inspect `view` and optionally return a replacement. Called at most
    /// once per outbound request.
    fn intercept<'a>(&'a self, view: &'a RequestView) -> SeamFuture<'a, Option<RequestReplacement>>;
}

/// Shared, cheap-to-clone alias for a request interceptor.
pub type SharedRequestInterceptor = Arc<dyn RequestInterceptor>;

/// A credential-free view of a failed request, handed to an
/// [`ErrorHook`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorView {
    /// The stable error class string, as produced by
    /// [`crate::retry::classify_error_class`]. This is the single shared
    /// vocabulary; callers compare it against their configured classes.
    pub error_class: String,
    /// The model the failed request used.
    pub model: String,
    /// The base URL the failed request targeted.
    pub base_url_alias: String,
    /// The zero-based attempt index at which the failure occurred.
    pub attempt: u32,
}

/// What the caller wants done after a provider/stream error.
///
/// The sampler itself only honors [`ErrorDirective::Fail`] (surface the
/// error without further internal retries). Model/base-URL substitution
/// requires resolving aliases to concrete endpoints and credentials,
/// which lives in the caller (the shell); the sampler never acts on
/// [`ErrorDirective::Retry`] directly. The variant is part of the shared
/// vocabulary so the shell's failover loop and any plugin directive
/// speak the same enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub enum ErrorDirective {
    /// Retry with an optionally substituted model and/or base-URL alias.
    /// Acted on by the caller's failover loop, not by the sampler.
    Retry {
        model: Option<String>,
        base_url_alias: Option<String>,
        max_attempts: Option<u32>,
    },
    /// Give up now: surface the error immediately.
    Fail,
    /// Do nothing special: fall through to the default behavior (the
    /// sampler's own retry classification, then the caller's built-in
    /// failover chain). The fail-open default.
    #[default]
    Passthrough,
}

// Tolerant deserialization: a directive that arrives as JSON from an
// untrusted programmable layer (a plugin) must never fail the pipeline.
// Unknown actions, missing fields, and outright garbage all fall open to
// `Passthrough`. Aliases mirror the reserved-event pattern (PascalCase /
// snake_case / camelCase).
impl<'de> Deserialize<'de> for ErrorDirective {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            action: Option<String>,
            #[serde(default)]
            model: Option<String>,
            #[serde(default, alias = "baseUrlAlias", alias = "base_url")]
            base_url_alias: Option<String>,
            #[serde(default, alias = "maxAttempts")]
            max_attempts: Option<u32>,
        }

        // Accept either a bare string ("fail" / "passthrough" / "retry")
        // or an object with an `action` field plus retry parameters.
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(s) = value.as_str() {
            return Ok(Self::from_action(s, None, None, None));
        }
        match serde_json::from_value::<Raw>(value) {
            Ok(raw) => Ok(Self::from_action(
                raw.action.as_deref().unwrap_or("passthrough"),
                raw.model,
                raw.base_url_alias,
                raw.max_attempts,
            )),
            // Shape we did not recognize at all -> fail open.
            Err(_) => Ok(Self::Passthrough),
        }
    }
}

impl ErrorDirective {
    fn from_action(
        action: &str,
        model: Option<String>,
        base_url_alias: Option<String>,
        max_attempts: Option<u32>,
    ) -> Self {
        match action.trim().to_ascii_lowercase().as_str() {
            "retry" => Self::Retry {
                model,
                base_url_alias,
                max_attempts,
            },
            "fail" => Self::Fail,
            // "passthrough", "", and every unknown action fall open.
            _ => Self::Passthrough,
        }
    }
}

/// Hook invoked by [`crate::SamplingClient`] on a provider/stream error.
///
/// Returning [`ErrorDirective::Passthrough`] (the fail-open default when
/// no hook is wired) leaves the sampler's own retry classification in
/// charge.
//
// The `Debug` bound is structural, as for [`RequestInterceptor`].
pub trait ErrorHook: Send + Sync + std::fmt::Debug {
    /// Inspect `view` and return a directive. Called at most once per
    /// surfaced error.
    fn on_error<'a>(&'a self, view: &'a ErrorView) -> SeamFuture<'a, ErrorDirective>;
}

/// Shared, cheap-to-clone alias for an error hook.
pub type SharedErrorHook = Arc<dyn ErrorHook>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_default_is_passthrough() {
        assert_eq!(ErrorDirective::default(), ErrorDirective::Passthrough);
    }

    #[test]
    fn directive_deserializes_object_forms() {
        let d: ErrorDirective = serde_json::from_str(
            r#"{"action":"retry","model":"m2","base_url_alias":"b2","max_attempts":5}"#,
        )
        .unwrap();
        assert_eq!(
            d,
            ErrorDirective::Retry {
                model: Some("m2".into()),
                base_url_alias: Some("b2".into()),
                max_attempts: Some(5),
            }
        );

        let d: ErrorDirective = serde_json::from_str(r#"{"action":"fail"}"#).unwrap();
        assert_eq!(d, ErrorDirective::Fail);
    }

    #[test]
    fn directive_deserializes_camel_aliases() {
        let d: ErrorDirective =
            serde_json::from_str(r#"{"action":"retry","baseUrlAlias":"b","maxAttempts":2}"#)
                .unwrap();
        assert_eq!(
            d,
            ErrorDirective::Retry {
                model: None,
                base_url_alias: Some("b".into()),
                max_attempts: Some(2),
            }
        );
    }

    #[test]
    fn directive_deserializes_bare_string() {
        let d: ErrorDirective = serde_json::from_str(r#""fail""#).unwrap();
        assert_eq!(d, ErrorDirective::Fail);
    }

    #[test]
    fn directive_unknown_action_falls_open_to_passthrough() {
        let d: ErrorDirective = serde_json::from_str(r#"{"action":"explode"}"#).unwrap();
        assert_eq!(d, ErrorDirective::Passthrough);
    }

    #[test]
    fn directive_garbage_falls_open_to_passthrough() {
        // An array is neither a string nor the object shape.
        let d: ErrorDirective = serde_json::from_str(r#"[1,2,3]"#).unwrap();
        assert_eq!(d, ErrorDirective::Passthrough);
        // Missing action defaults to passthrough.
        let d: ErrorDirective = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        assert_eq!(d, ErrorDirective::Passthrough);
    }
}
