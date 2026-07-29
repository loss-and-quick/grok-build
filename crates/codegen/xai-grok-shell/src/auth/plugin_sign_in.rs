//! The login-screen half of a plugin's interactive sign-in.
//!
//! A plugin's `start_oauth_flow` handler runs *before* any session exists — the
//! user is signing in, so there is no session, no session UI, and nothing to
//! publish a panel into. What does exist is the pre-auth screen the built-in
//! login already drives, through two session-independent channels owned by
//! [`crate::auth::single_flight::AuthSingleFlight`]:
//!
//! - a `oneshot` carrying the authorize URL, drained by `x.ai/auth/get_url`;
//! - an `mpsc` carrying codes the user submits, fed by `x.ai/auth/submit_code`.
//!
//! [`PluginSignInPrompt`] is the sidecar-facing end of exactly those two
//! channels: it implements [`xai_grok_plugin_host::SignInSink`], so the host's
//! `auth_publish_url` / `auth_await_code` RPCs land here and a plugin drives the
//! same screen the built-in flow does.
//!
//! # Why a process-wide instance
//!
//! There is one login screen per process, but the sidecar that serves the
//! sign-in may belong to *any* plugin host: the agent-level host built for a
//! session-less `/login`, or a live session's own host (which runs on that
//! session's OS thread). A single [`PluginSignInPrompt::global`] is installed on
//! every host, so whichever one runs the flow reaches the same screen. The
//! prompt itself holds nothing but channel endpoints, which is what lets it be
//! `Send + Sync` while `AuthSingleFlight` — `RefCell`-based and pinned to the
//! agent thread — stays exactly where it is.

use std::sync::{Arc, LazyLock, Mutex};

use super::flow::{AuthUrlInfo, AuthUrlMode};

/// Channel endpoints for the login attempt currently on screen. Empty slots
/// mean "no login is waiting": `publish_url` reports `false` and `await_code`
/// resolves `None`, so a plugin invoked outside `/login` can fall back to its
/// own UI instead of blocking on a screen nobody is looking at.
#[derive(Default)]
struct PromptState {
    /// Bumped by every [`PluginSignInPrompt::arm`]. Scopes the slot cleanup to
    /// the attempt that owns it, so a finishing predecessor cannot strip a
    /// successor's channels (the same generation discipline `AuthSingleFlight`
    /// uses for its own attempt state).
    generation: u64,
    /// Sends the authorize URL to `x.ai/auth/get_url`. Taken on first publish
    /// (the receiver is a one-shot).
    url_tx: Option<tokio::sync::oneshot::Sender<AuthUrlInfo>>,
    /// Receives codes submitted through `x.ai/auth/submit_code`. Moved out for
    /// the duration of a wait and restored afterwards, so a plugin can await
    /// again (e.g. after rejecting a mistyped code).
    code_rx: Option<tokio::sync::mpsc::Receiver<String>>,
}

/// The shell's [`xai_grok_plugin_host::SignInSink`]: bridges a sidecar's
/// `auth_publish_url` / `auth_await_code` onto the pre-auth screen's URL and
/// code channels.
#[derive(Default)]
pub(crate) struct PluginSignInPrompt {
    state: Mutex<PromptState>,
}

/// RAII disarm for one [`PluginSignInPrompt::arm`]: clears the attempt's
/// channels on drop, so an abandoned or cancelled login leaves the prompt
/// closed rather than accepting a URL for a screen that has moved on.
pub(crate) struct PluginSignInGuard<'a> {
    prompt: &'a PluginSignInPrompt,
    generation: u64,
}

impl Drop for PluginSignInGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.prompt.lock();
        if state.generation == self.generation {
            state.url_tx = None;
            state.code_rx = None;
        }
    }
}

impl PluginSignInPrompt {
    /// The process-wide prompt, installed on every plugin host so a sign-in
    /// reaches the login screen from whichever host serves it.
    pub(crate) fn global() -> Arc<Self> {
        static PROMPT: LazyLock<Arc<PluginSignInPrompt>> =
            LazyLock::new(|| Arc::new(PluginSignInPrompt::default()));
        Arc::clone(&PROMPT)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PromptState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Point the prompt at one login attempt's channels. The caller keeps the
    /// counterpart ends (`url_rx`, `code_tx`) in the [`AuthSingleFlight`]
    /// attempt, so the pre-auth screen's existing `get_url` / `submit_code`
    /// handlers serve a plugin sign-in unchanged.
    ///
    /// [`AuthSingleFlight`]: crate::auth::single_flight::AuthSingleFlight
    pub(crate) fn arm(
        &self,
        url_tx: tokio::sync::oneshot::Sender<AuthUrlInfo>,
        code_rx: tokio::sync::mpsc::Receiver<String>,
    ) -> PluginSignInGuard<'_> {
        let mut state = self.lock();
        state.generation = state.generation.wrapping_add(1);
        state.url_tx = Some(url_tx);
        state.code_rx = Some(code_rx);
        PluginSignInGuard {
            prompt: self,
            generation: state.generation,
        }
    }
}

impl xai_grok_plugin_host::SignInSink for PluginSignInPrompt {
    fn publish_url(&self, plugin: &str, url: String) -> bool {
        let url_tx = self.lock().url_tx.take();
        let Some(url_tx) = url_tx else {
            tracing::debug!(
                plugin,
                "plugin sign-in published a URL with no login waiting"
            );
            return false;
        };
        // `Loopback` is the presentation this flow needs: the screen shows the
        // URL with its copy affordance *and* the code box, which `await_code`
        // then drains. (`Device` deliberately hides the box.)
        url_tx
            .send(AuthUrlInfo {
                url,
                mode: AuthUrlMode::Loopback,
            })
            .is_ok()
    }

    fn await_code<'a>(
        &'a self,
        plugin: &'a str,
        timeout: Option<std::time::Duration>,
    ) -> xai_grok_plugin_host::OrchestratorFuture<'a, Option<String>> {
        Box::pin(async move {
            // Move the receiver out rather than holding the lock across the
            // wait: the mutex is a plain `std::sync` one (the prompt is armed
            // from the agent's synchronous `authenticate` path).
            let (generation, receiver) = {
                let mut state = self.lock();
                (state.generation, state.code_rx.take())
            };
            let Some(mut receiver) = receiver else {
                tracing::debug!(
                    plugin,
                    "plugin sign-in awaited a code with no login waiting"
                );
                return None;
            };
            let code = match timeout {
                Some(budget) => tokio::time::timeout(budget, receiver.recv())
                    .await
                    .ok()
                    .flatten(),
                // Unbounded here, but never actually unbounded: the hook's own
                // `start_oauth_flow` deadline bounds the whole flow, and
                // cancelling the login drops the sender, which ends the wait.
                None => receiver.recv().await,
            };
            // Restore for a retry, but only while this is still the attempt
            // that armed us — a superseded login must not get its channel back.
            let mut state = self.lock();
            if state.generation == generation && state.code_rx.is_none() {
                state.code_rx = Some(receiver);
            }
            code
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_plugin_host::SignInSink;

    /// An armed prompt plus the ends the login attempt keeps (`url_rx` feeds
    /// `x.ai/auth/get_url`, `code_tx` is fed by `x.ai/auth/submit_code`).
    fn armed(
        prompt: &PluginSignInPrompt,
    ) -> (
        PluginSignInGuard<'_>,
        tokio::sync::oneshot::Receiver<AuthUrlInfo>,
        tokio::sync::mpsc::Sender<String>,
    ) {
        let (url_tx, url_rx) = tokio::sync::oneshot::channel();
        let (code_tx, code_rx) = tokio::sync::mpsc::channel(1);
        (prompt.arm(url_tx, code_rx), url_rx, code_tx)
    }

    /// The published URL must reach the exact channel `x.ai/auth/get_url`
    /// drains, in the presentation that shows both the URL and the code box.
    #[tokio::test]
    async fn published_url_reaches_the_get_url_channel() {
        let prompt = PluginSignInPrompt::default();
        let (_guard, url_rx, _code_tx) = armed(&prompt);

        assert!(prompt.publish_url("acme", "https://example.test/authorize".into()));

        let info = url_rx.await.expect("the login attempt receives the URL");
        assert_eq!(info.url, "https://example.test/authorize");
        assert_eq!(info.mode, AuthUrlMode::Loopback);
    }

    /// A code submitted on the screen reaches the plugin's `await_code`.
    #[tokio::test]
    async fn submitted_code_reaches_the_plugin() {
        let prompt = PluginSignInPrompt::default();
        let (_guard, _url_rx, code_tx) = armed(&prompt);

        code_tx.send("123456".to_string()).await.expect("armed");
        assert_eq!(
            prompt.await_code("acme", None).await.as_deref(),
            Some("123456")
        );
    }

    /// A rejected code must not end the flow: the receiver is restored, so the
    /// plugin can ask for another one.
    #[tokio::test]
    async fn a_second_await_still_receives() {
        let prompt = PluginSignInPrompt::default();
        let (_guard, _url_rx, code_tx) = armed(&prompt);

        code_tx.send("wrong".to_string()).await.expect("armed");
        assert_eq!(
            prompt.await_code("acme", None).await.as_deref(),
            Some("wrong")
        );
        code_tx
            .send("right".to_string())
            .await
            .expect("still armed");
        assert_eq!(
            prompt.await_code("acme", None).await.as_deref(),
            Some("right")
        );
    }

    /// Outside a login there is no screen to drive: publish reports `false` and
    /// the wait resolves immediately, so a plugin can fall back to its own UI
    /// instead of blocking for its whole hook deadline.
    #[tokio::test]
    async fn idle_prompt_publishes_nothing_and_waits_for_nothing() {
        let prompt = PluginSignInPrompt::default();
        assert!(!prompt.publish_url("acme", "https://example.test/authorize".into()));
        assert!(prompt.await_code("acme", None).await.is_none());
    }

    /// Cancelling (dropping the guard) closes the prompt: a late publish from
    /// the abandoned flow must not paint a URL onto a screen that moved on.
    #[tokio::test]
    async fn disarming_closes_the_prompt() {
        let prompt = PluginSignInPrompt::default();
        let (guard, _url_rx, _code_tx) = armed(&prompt);
        drop(guard);

        assert!(!prompt.publish_url("acme", "https://example.test/authorize".into()));
        assert!(prompt.await_code("acme", None).await.is_none());
    }

    /// A predecessor's disarm must not strip the successor's channels — the
    /// same race `AuthSingleFlight`'s generations guard.
    #[tokio::test]
    async fn a_stale_disarm_leaves_the_successor_armed() {
        let prompt = PluginSignInPrompt::default();
        let (first, _url_rx, _code_tx) = armed(&prompt);
        let (_second, url_rx, code_tx) = armed(&prompt);

        drop(first);

        assert!(prompt.publish_url("acme", "https://example.test/second".into()));
        assert_eq!(
            url_rx.await.expect("successor still armed").url,
            "https://example.test/second"
        );
        code_tx.send("code".to_string()).await.expect("armed");
        assert_eq!(
            prompt.await_code("acme", None).await.as_deref(),
            Some("code")
        );
    }

    /// Cancelling a login drops the attempt's `code_tx`; the plugin's wait must
    /// end rather than hang until its hook deadline.
    #[tokio::test]
    async fn dropping_the_attempt_sender_ends_the_wait() {
        let prompt = PluginSignInPrompt::default();
        let (_guard, _url_rx, code_tx) = armed(&prompt);
        drop(code_tx);
        assert!(prompt.await_code("acme", None).await.is_none());
    }

    /// The whole wiring `authenticate` sets up for a `plugin-oauth:*` method,
    /// end to end: one [`AuthSingleFlight`] attempt owns the channels the ACP
    /// handlers use (`x.ai/auth/get_url` takes the URL receiver,
    /// `x.ai/auth/submit_code` pushes into the code sender) while the prompt
    /// holds their counterparts for the plugin. Proves the two halves are the
    /// same channels — the thing a per-half test cannot show.
    ///
    /// [`AuthSingleFlight`]: crate::auth::single_flight::AuthSingleFlight
    #[tokio::test]
    async fn the_login_attempt_and_the_prompt_share_one_pair_of_channels() {
        use crate::auth::single_flight::{AttemptChannels, AuthSingleFlight};

        let single_flight = AuthSingleFlight::default();
        let prompt = PluginSignInPrompt::default();

        let (url_tx, url_rx) = tokio::sync::oneshot::channel();
        let (code_tx, code_rx) = tokio::sync::mpsc::channel(1);
        let (_cancel, _guard) =
            single_flight.begin(Some(AttemptChannels::new(code_tx, url_rx)), Some(7));
        let _armed = prompt.arm(url_tx, code_rx);

        // The plugin publishes; `x.ai/auth/get_url` is what serves the screen.
        assert!(prompt.publish_url("acme", "https://example.test/authorize".into()));
        let url_rx = single_flight
            .take_url_rx()
            .expect("get_url must find the plugin's URL on the active attempt");
        assert_eq!(
            url_rx.await.expect("URL delivered").url,
            "https://example.test/authorize"
        );

        // The user pastes a code; `x.ai/auth/submit_code` is what receives it.
        single_flight
            .submit_code("123456".to_string())
            .expect("the attempt must accept a code for a plugin sign-in");
        assert_eq!(
            prompt.await_code("acme", None).await.as_deref(),
            Some("123456"),
            "the pasted code must reach the plugin's await"
        );
    }

    /// `x.ai/auth/cancel` cancels the attempt, which drops its channels; the
    /// plugin's wait must end instead of holding the flow to its hook deadline.
    #[tokio::test]
    async fn cancelling_the_login_ends_the_plugins_wait() {
        use crate::auth::single_flight::{AttemptChannels, AuthSingleFlight};

        let single_flight = AuthSingleFlight::default();
        let prompt = PluginSignInPrompt::default();
        let (url_tx, url_rx) = tokio::sync::oneshot::channel();
        let (code_tx, code_rx) = tokio::sync::mpsc::channel(1);
        let (cancel, _guard) =
            single_flight.begin(Some(AttemptChannels::new(code_tx, url_rx)), Some(1));
        let _armed = prompt.arm(url_tx, code_rx);

        single_flight.cancel_for_client_seq(1);
        assert!(cancel.is_cancelled());
        assert!(prompt.await_code("acme", None).await.is_none());
    }

    /// `timeout_ms` bounds the wait on its own.
    #[tokio::test]
    async fn await_code_honours_its_timeout() {
        let prompt = PluginSignInPrompt::default();
        let (_guard, _url_rx, _code_tx) = armed(&prompt);
        assert!(
            prompt
                .await_code("acme", Some(std::time::Duration::from_millis(20)))
                .await
                .is_none()
        );
    }
}
