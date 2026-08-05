//! `Send + Sync` seam onto the session's bounded auxiliary failover loop.
//!
//! [`SessionActor`] holds `Cell`/`RefCell` state, so it is not `Sync` and cannot
//! be reached from a plain `tokio::spawn` task. Session-title generation runs on
//! exactly such a task — the persistence actor spawns it so an inference never
//! stalls a storage write — which is why titling was the one auxiliary caller
//! `[[model_fallbacks]]` did not reach: it held a client and had no way to ask
//! anyone to route a second one.
//!
//! [`ChannelWebContentDistiller`] already crosses this boundary, but it crosses
//! it as the `web_fetch` tool's own trait: catalog questions answered in place,
//! one `infer` forwarded. A second caller needing the same crossing is what
//! makes the crossing itself worth naming, so this module is the caller-agnostic
//! half — a `Send + Sync` handle that carries a whole [`ConversationRequest`] to
//! the session's `LocalSet`, where it is routed and issued through
//! [`SessionActor::aux_collect_with_fallback`].
//!
//! [`ChannelWebContentDistiller`]: super::web_fetch_distill::ChannelWebContentDistiller
//!
//! # What crosses, and what does not
//!
//! A catalog pin and a request. Never a client, a credential or a resolved
//! endpoint: those are derived on the session side, at call time, from the live
//! catalog and auth state. That is the whole difference between this and
//! snapshotting a client onto the caller — a snapshot taken at session open
//! outlives a `/model` switch and a token refresh with nothing to say so, and it
//! can only ever reach the one endpoint it was built for, which is also why it
//! could never hop.
//!
//! # A pin the catalog cannot place degrades to the session's own route
//!
//! Not a skip, unlike `web_fetch` distillation. The callers here name a helper
//! slug that defaults to a compiled-in first-party model, and forcing that slug
//! onto a custom `[[provider]]`'s endpoint only 404s. The session's model is the
//! one slug the session's endpoint is known to serve, so a miss falls to it —
//! the policy `finalize_aux_sampler_config` already applied to session titles
//! and image description. A chain written against the *pin* is therefore never
//! entered on a miss; only one written against the session model applies, which
//! is the same rule the unpinned Auto-mode classifier follows.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use xai_grok_sampling_types::{ConversationRequest, ConversationResponse};

use super::SessionActor;

/// What a caller asks of the bridge, apart from the request itself.
#[derive(Clone)]
pub(crate) struct AuxCall {
    /// Names the feature on the failover debug log, so a one-shot that quietly
    /// stopped working is attributable.
    pub(crate) caller: &'static str,
    /// The configured model, as a catalog key where the operator qualified it.
    /// Passed on as written: the resolver looks a key up before it falls back to
    /// a bare-slug scan, so a pin qualified to one of two providers serving the
    /// same slug keeps naming that provider's entry, endpoint and credential.
    pub(crate) model_pin: String,
    /// Whether the request takes the provider admission gate's background lane.
    pub(crate) background: bool,
    /// This caller's hop budget. Per caller, never one number everywhere; see
    /// the constants in [`super::aux_fallback`].
    pub(crate) max_hops: u32,
}

/// One auxiliary inference, handed from a `Send` task to the session's
/// `LocalSet`.
struct AuxJob {
    call: AuxCall,
    request: ConversationRequest,
    respond_to: oneshot::Sender<Result<ConversationResponse, String>>,
}

/// The `Send + Sync` half of the seam: a channel to the session's own task.
///
/// Cloneable and cheap, so one bridge serves every caller on the far side of the
/// boundary rather than one channel per feature.
#[derive(Clone, Debug)]
pub struct AuxInferenceBridge {
    jobs: mpsc::UnboundedSender<AuxJob>,
}

impl AuxInferenceBridge {
    /// Route and issue `request` on the session, hopping onto the operator's
    /// `[[model_fallbacks]]` chain within `call.max_hops`.
    ///
    /// `request.model` is overwritten with the routing slug of whatever entry
    /// the pin resolved to: one resolution produces both the client and the
    /// slug, so the endpoint a request travels to and the model it names cannot
    /// come from different entries.
    pub(crate) async fn collect(
        &self,
        call: AuxCall,
        request: ConversationRequest,
    ) -> Result<ConversationResponse, String> {
        let (respond_to, answer) = oneshot::channel();
        self.jobs
            .send(AuxJob {
                call,
                request,
                respond_to,
            })
            .map_err(|_| "the session's auxiliary inference task is gone".to_owned())?;
        answer
            .await
            .map_err(|_| "auxiliary inference job was dropped".to_owned())?
    }
}

impl SessionActor {
    /// Mint a bridge served by a task on this session's `LocalSet`.
    ///
    /// The task holds a `Weak`, not an `Arc`: the bridge is handed to actors the
    /// session outlives but does not own, and an owning handle would keep a torn
    /// down session alive for as long as one of them kept its sender. A session
    /// that is already gone answers with an error, which every caller here
    /// degrades on.
    pub(crate) fn spawn_aux_inference_bridge(self: &Arc<Self>) -> AuxInferenceBridge {
        let (jobs, mut rx) = mpsc::unbounded_channel::<AuxJob>();
        let weak = Arc::downgrade(self);
        tokio::task::spawn_local(async move {
            while let Some(job) = rx.recv().await {
                let result = match weak.upgrade() {
                    Some(session) => session.run_aux_job(&job.call, job.request).await,
                    None => Err("the session is no longer running".to_owned()),
                };
                if let Err(error) = &result {
                    tracing::debug!(
                        caller = job.call.caller,
                        model = %job.call.model_pin,
                        %error,
                        "auxiliary inference failed; the caller degrades"
                    );
                }
                let _ = job.respond_to.send(result);
            }
            tracing::debug!("aux inference bridge task exiting (channel closed)");
        });
        AuxInferenceBridge { jobs }
    }

    /// Resolve one job's route and issue it under the caller's hop budget.
    async fn run_aux_job(
        &self,
        call: &AuxCall,
        mut request: ConversationRequest,
    ) -> Result<ConversationResponse, String> {
        let route = match self
            .resolve_aux_route(&call.model_pin, call.background)
            .await
        {
            Some(route) => route,
            // The pin has no route of its own. Degrading onto the session's
            // client is safe here only because the request is re-stamped with
            // the session's own wire model below: one endpoint, one credential,
            // one slug, all from the same place.
            None => self
                .session_client_aux_route(call.background)
                .await
                .map_err(|e| e.to_string())?,
        };
        request.model = Some(route.wire_model.clone());
        self.aux_collect_with_fallback(call.caller, route, request, call.max_hops)
            .await
            .map_err(|e| e.to_string())
    }
}
