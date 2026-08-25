//! Admission control for providers that enforce a hard parallelism limit.
//!
//! Some endpoints cap how many requests they will serve at once *per model*
//! and **reject** the surplus rather than queueing it. The cap is a static
//! fact of the endpoint, so it is declared in config
//! (`[[provider]] max_concurrent`) and threaded down to
//! [`crate::SamplerConfig::max_concurrent`]; this module turns that number
//! into a gate every request must pass.
//!
//! # Why the gate lives here
//!
//! Every inference request in the workspace — the main turn, subagent turns,
//! and each auxiliary one-shot (session title, image description, the Auto-mode
//! classifier, `web_fetch` distillation, prompt suggestion, the goal evaluator,
//! memory consolidation) — is issued by a [`crate::SamplingClient`]. Those
//! clients are
//! *not* shared: the aux paths each build their own from a separately resolved
//! `SamplerConfig`. A limiter owned by a client would therefore cap only the
//! traffic that happened to share it, which is the one thing worse than no
//! limiter: it looks like a cap and is not one. So the semaphores live in a
//! process-wide registry keyed by the endpoint, and every client finds the same
//! gate for the same endpoint.
//!
//! # Keying
//!
//! By `base_url` plus the *wire* model, not by the catalog key. The provider
//! meters an endpoint, and two catalog entries can name one endpoint — a
//! `<provider_id>/<model>` key and its bare-slug alias always do, and two
//! `[[provider]]` entries may point at the same host. Keying by config name
//! would hand each alias its own cap and admit a multiple of the real limit.
//!
//! # Release
//!
//! [`AdmissionPermit`] is a plain RAII guard over
//! [`tokio::sync::OwnedSemaphorePermit`]. There is no `release()` to forget to
//! call: success, every error path, the admission timeout, a panic, and
//! cancellation (the request future dropped mid-await, or the returned response
//! stream dropped mid-body) all release by dropping the value. For streaming
//! requests the permit is moved into the returned stream by
//! [`hold_permit_for_stream`], because the provider counts a request as
//! in-flight until its body is done, not until its headers arrive.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use xai_grok_sampling_types::{Result, SamplingError};

/// How a request competes for a saturated endpoint's slots.
///
/// The split is "is a turn blocked on this", not "is this an auxiliary model".
/// Most auxiliary one-shots run *inside* a turn and hold it up — an image
/// description gates prompt build, a permission classifier gates a tool call, a
/// distillation gates a tool result — so demoting them by kind would slow the
/// very turn the lane exists to protect. What deserves demoting is work that is
/// fire-and-forget: titles, prompt suggestions, memory consolidation. Those are
/// also the only ones that burst, several at once against a cap of two or
/// three, which is exactly the case a plain arrival-order queue handles badly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyClass {
    /// A request some turn is blocked on. The default, so a call site that is
    /// never classified is treated as user-visible rather than silently
    /// deprioritized.
    #[default]
    Interactive,
    /// Fire-and-forget inference nothing waits on.
    Background,
}

/// Log once admission has been waiting this long, so a saturated endpoint is
/// diagnosable from the logs rather than looking like a hang.
const WARN_ADMISSION_WAIT: Duration = Duration::from_secs(5);

/// Give up waiting for a slot after this long and fail the request.
///
/// Deliberately far longer than any legitimate queue: with a small cap and
/// several sessions a request can honestly wait minutes behind long streaming
/// turns, and converting that into a spurious failure would be worse than
/// waiting. The deadline is insurance against the one failure this module must
/// not have — a permit that is never returned, which would otherwise wedge the
/// endpoint silently and permanently. With it, a leak surfaces as a named error
/// naming the endpoint instead of a process that stops talking to the provider.
const ADMISSION_DEADLINE: Duration = Duration::from_secs(600);

/// Admission gate for one `(endpoint, wire model)` pair.
///
/// Two semaphores rather than one because the interactive class needs a slot
/// reserved for it. `all` is the real cap; `background` additionally bounds how
/// many of those slots background work may hold, at `max - 1` whenever the cap
/// leaves room for a reservation. A background request takes `background` first
/// and `all` second — always that order, so there is no lock-ordering cycle —
/// which means at most `max - 1` background requests can be in flight *or*
/// queued on `all` at any moment. An interactive request therefore waits behind
/// a bounded number of background requests no matter how large the background
/// burst is, instead of behind the whole burst.
#[derive(Debug)]
struct ProviderGate {
    key: String,
    all: Arc<Semaphore>,
    background: Arc<Semaphore>,
    /// The cap this gate was built with, kept only to report a later,
    /// conflicting declaration.
    declared_max: NonZeroUsize,
}

/// RAII admission slot. Dropping it returns the slot to the gate.
///
/// The fields are never read: holding them *is* the whole behaviour, and
/// dropping the value is the only way to release. That is deliberate — there is
/// no API by which a caller can release early and then keep issuing, and no
/// path that can forget to release.
#[derive(Debug)]
pub struct AdmissionPermit {
    _all: OwnedSemaphorePermit,
    /// Present only for [`ConcurrencyClass::Background`].
    _background: Option<OwnedSemaphorePermit>,
}

impl ProviderGate {
    fn new(key: String, max: NonZeroUsize) -> Self {
        // One slot held back for interactive work whenever the cap has more
        // than one. At `max == 1` there is nothing to reserve and the two
        // classes simply share the single slot in arrival order.
        let background_lanes = max.get().saturating_sub(1).max(1);
        Self {
            key,
            all: Arc::new(Semaphore::new(max.get())),
            background: Arc::new(Semaphore::new(background_lanes)),
            declared_max: max,
        }
    }

    async fn acquire(&self, class: ConcurrencyClass) -> Result<AdmissionPermit> {
        let started = Instant::now();
        let background = match class {
            ConcurrencyClass::Interactive => None,
            ConcurrencyClass::Background => Some(
                self.acquire_one(&self.background, started, "background lane")
                    .await?,
            ),
        };
        let all = self.acquire_one(&self.all, started, "slot").await?;
        let waited = started.elapsed();
        if waited >= WARN_ADMISSION_WAIT {
            tracing::warn!(
                endpoint = %self.key,
                max_concurrent = self.declared_max.get(),
                waited_secs = waited.as_secs(),
                ?class,
                "waited for a provider concurrency slot"
            );
        }
        Ok(AdmissionPermit {
            _all: all,
            _background: background,
        })
    }

    async fn acquire_one(
        &self,
        sem: &Arc<Semaphore>,
        started: Instant,
        what: &str,
    ) -> Result<OwnedSemaphorePermit> {
        let remaining = ADMISSION_DEADLINE.saturating_sub(started.elapsed());
        match tokio::time::timeout(remaining, Arc::clone(sem).acquire_owned()).await {
            // `acquire_owned` only errors on a closed semaphore, and these are
            // never closed.
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(saturated(&self.key, self.declared_max, started.elapsed())),
            Err(_) => {
                tracing::error!(
                    endpoint = %self.key,
                    max_concurrent = self.declared_max.get(),
                    what,
                    "gave up waiting for a provider concurrency slot"
                );
                Err(saturated(&self.key, self.declared_max, started.elapsed()))
            }
        }
    }
}

fn saturated(key: &str, max: NonZeroUsize, waited: Duration) -> SamplingError {
    SamplingError::Api {
        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        message: format!(
            "no concurrency slot for `{key}` after {}s: max_concurrent = {} and every slot is \
             still held",
            waited.as_secs(),
            max.get(),
        ),
        model_metadata: None,
        retry_after_secs: None,
        // Retrying would re-enter the same queue and wait the same deadline
        // again; the caller should surface this rather than spend its budget.
        should_retry: Some(false),
        // Synthesised locally: no provider envelope to read a code from.
        error_code: None,
    }
}

type GateRegistry = Mutex<HashMap<String, Arc<ProviderGate>>>;

fn registry() -> &'static GateRegistry {
    static GATES: OnceLock<GateRegistry> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Endpoint identity: base URL (trailing slashes normalized away, since
/// `https://h/v1` and `https://h/v1/` are one endpoint) plus the wire model.
/// The separator is a unit separator so it cannot occur in either part.
fn gate_key(base_url: &str, model: &str) -> String {
    format!("{}\u{1f}{model}", base_url.trim_end_matches('/'))
}

fn gate_for(base_url: &str, model: &str, max: NonZeroUsize) -> Arc<ProviderGate> {
    let key = gate_key(base_url, model);
    let mut gates = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = gates.get(&key) {
        if existing.declared_max != max {
            // A semaphore cannot shrink below what is already in flight without
            // either blocking or over-admitting, so the first declaration owns
            // the gate for the life of the process. The cap describes the
            // provider, not the session, so this only bites when the config is
            // edited mid-run — where the honest answer is that it takes effect
            // on restart, said out loud rather than silently.
            tracing::warn!(
                endpoint = %key,
                in_effect = existing.declared_max.get(),
                declared = max.get(),
                "max_concurrent was already established for this endpoint; the new value takes \
                 effect on restart"
            );
        }
        return Arc::clone(existing);
    }
    let gate = Arc::new(ProviderGate::new(key.clone(), max));
    gates.insert(key, Arc::clone(&gate));
    gate
}

/// Wait for a slot on `(base_url, model)`, or return immediately when the
/// endpoint declares no cap.
///
/// `Ok(None)` is the uncapped case and costs nothing: no registry lookup, no
/// semaphore, no allocation.
pub(crate) async fn admit(
    base_url: &str,
    model: &str,
    max: Option<NonZeroUsize>,
    class: ConcurrencyClass,
) -> Result<Option<AdmissionPermit>> {
    let Some(max) = max else {
        return Ok(None);
    };
    gate_for(base_url, model, max)
        .acquire(class)
        .await
        .map(Some)
}

/// Move `permit` into `stream` so the slot is held until the response body ends
/// or the consumer drops the stream.
///
/// A streaming request is in flight, from the provider's point of view, for as
/// long as its body is open. Releasing when the headers arrive would let the
/// process run arbitrarily far over the cap.
pub(crate) fn hold_permit_for_stream<T: Send + 'static>(
    stream: BoxStream<'static, T>,
    permit: Option<AdmissionPermit>,
) -> BoxStream<'static, T> {
    let Some(permit) = permit else {
        return stream;
    };
    futures_util::stream::unfold((stream, Some(permit)), |(mut stream, permit)| async move {
        match stream.next().await {
            Some(item) => Some((item, (stream, permit))),
            // End of body: hand the slot back now rather than waiting for
            // the consumer to drop an exhausted stream.
            None => {
                drop(permit);
                None
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Each test needs an endpoint nothing else in the process shares, since
    /// the registry is process-wide by design.
    fn unique_base_url() -> String {
        static N: AtomicU32 = AtomicU32::new(0);
        format!("https://gate-{}.test/v1", N.fetch_add(1, Ordering::Relaxed))
    }

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    /// The declared cap is the number of requests that can hold a slot at once;
    /// the next one waits rather than proceeding.
    #[tokio::test]
    async fn cap_admits_exactly_max_concurrent() {
        let base = unique_base_url();
        let one = admit(&base, "m", Some(nz(2)), ConcurrencyClass::Interactive)
            .await
            .unwrap();
        let two = admit(&base, "m", Some(nz(2)), ConcurrencyClass::Interactive)
            .await
            .unwrap();

        let third = tokio::time::timeout(
            Duration::from_millis(50),
            admit(&base, "m", Some(nz(2)), ConcurrencyClass::Interactive),
        )
        .await;
        assert!(third.is_err(), "a third request must not be admitted");

        drop(one);
        let third = tokio::time::timeout(
            Duration::from_secs(1),
            admit(&base, "m", Some(nz(2)), ConcurrencyClass::Interactive),
        )
        .await
        .expect("releasing a slot must admit the waiter");
        assert!(third.is_ok());
        drop(two);
    }

    /// The gate is per `(endpoint, wire model)`: a second model on the same
    /// endpoint has its own slots, because the provider meters them separately.
    #[tokio::test]
    async fn separate_models_do_not_share_slots() {
        let base = unique_base_url();
        let _held = admit(&base, "m1", Some(nz(1)), ConcurrencyClass::Interactive)
            .await
            .unwrap();
        let other = tokio::time::timeout(
            Duration::from_millis(50),
            admit(&base, "m2", Some(nz(1)), ConcurrencyClass::Interactive),
        )
        .await;
        assert!(other.is_ok(), "a different wire model has its own cap");
    }

    /// Two spellings of one endpoint share one gate. A catalog exposes the same
    /// provider entry under `<id>/<model>` and the bare slug, and both resolve
    /// to this base URL — if they did not share, the process would admit twice
    /// the declared cap.
    #[tokio::test]
    async fn trailing_slash_spellings_share_one_gate() {
        let base = unique_base_url();
        let _held = admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive)
            .await
            .unwrap();
        let same = tokio::time::timeout(
            Duration::from_millis(50),
            admit(
                &format!("{base}/"),
                "m",
                Some(nz(1)),
                ConcurrencyClass::Interactive,
            ),
        )
        .await;
        assert!(same.is_err(), "`{base}` and `{base}/` are one endpoint");
    }

    /// An undeclared cap costs nothing and admits everything.
    #[tokio::test]
    async fn no_declared_cap_admits_unconditionally() {
        let base = unique_base_url();
        let mut held = Vec::new();
        for _ in 0..32 {
            let permit = tokio::time::timeout(
                Duration::from_millis(50),
                admit(&base, "m", None, ConcurrencyClass::Interactive),
            )
            .await
            .expect("an uncapped endpoint never waits")
            .unwrap();
            assert!(permit.is_none());
            held.push(permit);
        }
    }

    /// A background burst cannot take the last slot. This is the property the
    /// reserved lane exists for: however many aux jobs pile up, the turn the
    /// user is watching finds a slot after at most the in-flight requests
    /// finish, not after the whole burst drains.
    #[tokio::test]
    async fn background_cannot_occupy_the_last_slot() {
        let base = unique_base_url();
        let mut background = Vec::new();
        for _ in 0..2 {
            background.push(
                tokio::time::timeout(
                    Duration::from_millis(50),
                    admit(&base, "m", Some(nz(3)), ConcurrencyClass::Background),
                )
                .await
                .expect("background fills its own lane")
                .unwrap(),
            );
        }
        let third_background = tokio::time::timeout(
            Duration::from_millis(50),
            admit(&base, "m", Some(nz(3)), ConcurrencyClass::Background),
        )
        .await;
        assert!(
            third_background.is_err(),
            "background is capped at max - 1 even though a slot is free"
        );

        let interactive = tokio::time::timeout(
            Duration::from_millis(50),
            admit(&base, "m", Some(nz(3)), ConcurrencyClass::Interactive),
        )
        .await;
        assert!(
            interactive.is_ok(),
            "the reserved slot must still be there for interactive work"
        );
        drop(background);
    }

    /// With a cap of one there is nothing to reserve, so background work may
    /// use the single slot rather than deadlocking against an empty lane.
    #[tokio::test]
    async fn a_cap_of_one_still_admits_background() {
        let base = unique_base_url();
        let permit = tokio::time::timeout(
            Duration::from_millis(50),
            admit(&base, "m", Some(nz(1)), ConcurrencyClass::Background),
        )
        .await
        .expect("background must not be locked out at max_concurrent = 1");
        assert!(permit.is_ok());
    }

    /// Dropping the acquire future before it resolves must not consume a slot.
    /// A cancelled waiter that kept its place would leak the cap away one
    /// cancelled request at a time.
    #[tokio::test]
    async fn a_cancelled_waiter_leaves_the_gate_intact() {
        let base = unique_base_url();
        let held = admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive)
            .await
            .unwrap();

        for _ in 0..8 {
            let waiter = admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive);
            assert!(
                tokio::time::timeout(Duration::from_millis(20), waiter)
                    .await
                    .is_err(),
                "the slot is held, so every waiter must be dropped mid-wait"
            );
        }

        drop(held);
        let after = tokio::time::timeout(
            Duration::from_millis(50),
            admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive),
        )
        .await
        .expect("the single slot must still be grantable");
        assert!(after.is_ok());
    }

    /// A permit moved into a stream is released when the stream is dropped
    /// part-way through — the cancellation shape a streaming turn actually
    /// takes when the user hits escape.
    #[tokio::test]
    async fn dropping_a_stream_mid_body_releases_the_slot() {
        let base = unique_base_url();
        let permit = admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive)
            .await
            .unwrap();
        let raw = futures_util::stream::iter(vec![1u8, 2, 3]).boxed();
        let mut held = hold_permit_for_stream(raw, permit);

        assert_eq!(held.next().await, Some(1));
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive)
            )
            .await
            .is_err(),
            "the slot is held for as long as the body is open"
        );

        drop(held);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive)
            )
            .await
            .is_ok(),
            "dropping the stream mid-body must return the slot"
        );
    }

    /// Draining a stream to its end releases the slot without waiting for the
    /// consumer to drop the exhausted stream.
    #[tokio::test]
    async fn exhausting_a_stream_releases_the_slot() {
        let base = unique_base_url();
        let permit = admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive)
            .await
            .unwrap();
        let raw = futures_util::stream::iter(vec![1u8, 2]).boxed();
        let mut held = hold_permit_for_stream(raw, permit);
        while held.next().await.is_some() {}

        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive)
            )
            .await
            .is_ok(),
            "an exhausted stream must not keep holding the slot"
        );
        // `held` is deliberately still alive here.
        drop(held);
    }

    /// The first declaration owns the gate; a conflicting one does not silently
    /// widen the cap.
    #[tokio::test]
    async fn a_later_conflicting_declaration_does_not_widen_the_cap() {
        let base = unique_base_url();
        let _held = admit(&base, "m", Some(nz(1)), ConcurrencyClass::Interactive)
            .await
            .unwrap();
        let widened = tokio::time::timeout(
            Duration::from_millis(50),
            admit(&base, "m", Some(nz(8)), ConcurrencyClass::Interactive),
        )
        .await;
        assert!(
            widened.is_err(),
            "a second, larger declaration must not create a second gate"
        );
    }
}
