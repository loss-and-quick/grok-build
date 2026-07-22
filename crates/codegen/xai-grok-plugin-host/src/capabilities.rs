//! The plugin→core capability server.
//!
//! Every inbound request/notification a sidecar sends is dispatched here:
//!
//! - `log_emit`     → forwarded to `tracing` with a `plugin` field.
//! - `storage_*`    → a per-plugin JSON KV store, core-locked and atomically
//!   persisted (this is the feature that kills the self-rolled
//!   store+mutex+GC in the user's taskboard/memory/council plugins).
//! - `config_get`   → returns the plugin config handed to the host at
//!   registration.
//! - `agent_*`      → reserved orchestration; replies `method_not_found`.
//!
//! One [`PluginCapabilities`] is built per registered plugin and shared with
//! every sidecar spawned for it (across restarts), so storage state survives a
//! crash-restart.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use xai_grok_plugin_protocol::{
    AgentCancelOutcomeDto, AgentCancelParams, AgentCancelResult, AgentDescriptorDto, AgentEventDto,
    AgentEventKindDto, AgentEventsParams, AgentEventsResult, AgentListResult, AgentSpawnParams,
    AgentSpawnResult,
    AgentStatusDto, AgentWaitParams, AgentWaitResult, ConfigGetResult, LogEmitParams, LogLevelDto,
    StorageDeleteParams, StorageDeleteResult, StorageGetParams, StorageGetResult,
    StorageListParams, StorageListResult, StorageSetParams, StorageSetResult,
};

use crate::orchestration::{
    AgentOrchestrator, AgentOutcome, AgentProgress, AgentSpawnSpec, OrchestratorCancel,
};
use crate::rpc::RpcError;

/// Default `agent_wait` deadline when the plugin passes no `timeout_ms`.
const AGENT_WAIT_DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Coordinator poll cadence while an `agent_events` long-poll is held open.
const AGENT_EVENTS_POLL_INTERVAL: Duration = Duration::from_millis(400);
/// Cap on buffered events per subagent; oldest entries drop first (a lagging
/// consumer observes a seq gap rather than unbounded memory).
const AGENT_EVENT_BUFFER_CAP: usize = 512;
/// After a per-spawn timeout fires and the subagent is cancelled, how long the
/// watcher waits for the real terminal result before synthesizing one.
const AGENT_TIMEOUT_CANCEL_GRACE: Duration = Duration::from_secs(10);

/// Per-plugin capability context. Cheap to `Arc`-clone into each sidecar.
pub struct PluginCapabilities {
    name: String,
    config: Value,
    storage: PluginStorage,
    /// The injected orchestration seam; unset until the shell wires one, in
    /// which case every `agent_*` method answers `method_not_found` (exactly
    /// the pre-wiring behavior, so plugins can feature-detect).
    orchestrator: std::sync::OnceLock<Arc<dyn AgentOrchestrator>>,
    /// Subagents spawned by THIS plugin, keyed by id. Lives here (not on the
    /// sidecar) so wait/events state survives a sidecar crash-restart, and so
    /// one plugin can never wait on or cancel another plugin's spawns.
    agents: std::sync::Mutex<HashMap<String, Arc<AgentHandle>>>,
}

impl PluginCapabilities {
    pub fn new(name: String, config: Value, storage_path: PathBuf) -> Self {
        Self {
            name,
            config,
            storage: PluginStorage::new(storage_path),
            orchestrator: std::sync::OnceLock::new(),
            agents: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Install the orchestration seam. First call wins; later calls are
    /// ignored (one session, one coordinator).
    pub fn set_orchestrator(&self, orchestrator: Arc<dyn AgentOrchestrator>) {
        let _ = self.orchestrator.set(orchestrator);
    }

    /// Serve a plugin request, returning the JSON `result` or a JSON-RPC error.
    pub async fn handle_request(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "storage_get" => {
                let p: StorageGetParams = parse_params(params)?;
                let value = self.storage.get(&p.key).await.map_err(storage_err)?;
                to_result(&StorageGetResult { value })
            }
            "storage_set" => {
                let p: StorageSetParams = parse_params(params)?;
                self.storage
                    .set(p.key, p.value)
                    .await
                    .map_err(storage_err)?;
                to_result(&StorageSetResult {})
            }
            "storage_delete" => {
                let p: StorageDeleteParams = parse_params(params)?;
                let existed = self.storage.delete(&p.key).await.map_err(storage_err)?;
                to_result(&StorageDeleteResult { existed })
            }
            "storage_list" => {
                let p: StorageListParams = parse_params(params)?;
                let keys = self
                    .storage
                    .list(p.prefix.as_deref())
                    .await
                    .map_err(storage_err)?;
                to_result(&StorageListResult { keys })
            }
            "config_get" => to_result(&ConfigGetResult {
                value: self.config.clone(),
            }),
            "agent_spawn" => self.agent_spawn(params).await,
            "agent_wait" => self.agent_wait(params).await,
            "agent_events" => self.agent_events(params).await,
            "agent_list" => self.agent_list().await,
            "agent_cancel" => self.agent_cancel(params).await,
            // Reserved: superseded by the cursor-based `agent_events` poll
            // (request/reply framing; state survives sidecar restarts).
            "agent_events_subscribe" => Err(RpcError::method_not_found(method)),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    /// The orchestrator, or the pre-wiring `method_not_found` answer that lets
    /// plugins feature-detect orchestration support.
    fn orchestrator_for(&self, method: &str) -> Result<Arc<dyn AgentOrchestrator>, RpcError> {
        self.orchestrator
            .get()
            .cloned()
            .ok_or_else(|| RpcError::method_not_found(method))
    }

    /// A handle for one of THIS plugin's spawns; foreign/unknown ids are
    /// invalid params (ids are not shared across plugins by design).
    fn agent_handle(&self, id: &str) -> Result<Arc<AgentHandle>, RpcError> {
        self.agents
            .lock()
            .expect("agents map poisoned")
            .get(id)
            .cloned()
            .ok_or_else(|| {
                RpcError::invalid_params(format!("unknown subagent id '{id}' for this plugin"))
            })
    }

    async fn agent_spawn(&self, params: &Value) -> Result<Value, RpcError> {
        // Orchestrator first in every `agent_*` arm: an unwired host answers
        // `method_not_found` regardless of params, so feature detection never
        // depends on payload shape.
        let orchestrator = self.orchestrator_for("agent_spawn")?;
        let p: AgentSpawnParams = parse_params(params)?;
        let spec = AgentSpawnSpec {
            plugin: self.name.clone(),
            agent_type: p.agent_type,
            prompt: p.prompt,
            description: p.description,
            model: p.model,
            cwd: p.cwd,
        };
        let spawn_data = json!({
            "agent_type": spec.agent_type,
            "description": spec.description,
            "model": spec.model,
            "cwd": spec.cwd,
            "prompt_chars": spec.prompt.chars().count(),
            "timeout_ms": p.timeout_ms,
        });
        let spawned = orchestrator.spawn(spec).map_err(RpcError::internal)?;
        let handle = Arc::new(AgentHandle::default());
        handle.push_event(AgentEventKindDto::Spawned, spawn_data);
        self.agents
            .lock()
            .expect("agents map poisoned")
            .insert(spawned.id.clone(), Arc::clone(&handle));
        spawn_outcome_watcher(
            orchestrator,
            spawned.id.clone(),
            spawned.result_rx,
            handle,
            p.timeout_ms,
        );
        to_result(&AgentSpawnResult { id: spawned.id })
    }

    async fn agent_wait(&self, params: &Value) -> Result<Value, RpcError> {
        self.orchestrator_for("agent_wait")?;
        let p: AgentWaitParams = parse_params(params)?;
        let handle = self.agent_handle(&p.id)?;
        let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(AGENT_WAIT_DEFAULT_TIMEOUT_MS));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register for wakeups BEFORE the outcome check so a completion
            // racing this loop can't be missed.
            let notified = handle.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(outcome) = handle.outcome_snapshot() {
                return to_result(&wait_result_of(&outcome));
            }
            if tokio::time::Instant::now() >= deadline {
                // Not terminal within the budget: report running (poll again
                // or cancel), never an error.
                return to_result(&AgentWaitResult {
                    status: AgentStatusDto::Running,
                    output: None,
                    error: None,
                    tokens_used: 0,
                    duration_ms: 0,
                    tool_calls: 0,
                    turns: 0,
                });
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(deadline) => {}
            }
        }
    }

    async fn agent_events(&self, params: &Value) -> Result<Value, RpcError> {
        let orchestrator = self.orchestrator_for("agent_events")?;
        let p: AgentEventsParams = parse_params(params)?;
        let handle = self.agent_handle(&p.id)?;
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(p.timeout_ms.unwrap_or(0));
        loop {
            let notified = handle.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            // While running, synthesize a `progress` event whenever the
            // coordinator's live counters changed since the last look.
            if handle.outcome_snapshot().is_none()
                && let Some(progress) = orchestrator.progress(&p.id).await
                && handle.record_progress(&progress)
            {
                handle.push_event(
                    AgentEventKindDto::Progress,
                    json!({
                        "phase": progress.phase,
                        "turns": progress.turns,
                        "tool_calls": progress.tool_calls,
                        "tokens_used": progress.tokens_used,
                        "elapsed_ms": progress.elapsed_ms,
                    }),
                );
            }
            let (events, next_cursor) = handle.events_since(p.cursor);
            let done = handle.terminal_pushed.load(Ordering::Acquire);
            if !events.is_empty() || done || tokio::time::Instant::now() >= deadline {
                return to_result(&AgentEventsResult {
                    events,
                    next_cursor,
                    done,
                });
            }
            let next_poll = tokio::time::Instant::now() + AGENT_EVENTS_POLL_INTERVAL;
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(next_poll.min(deadline)) => {}
            }
        }
    }

    async fn agent_list(&self) -> Result<Value, RpcError> {
        let orchestrator = self.orchestrator_for("agent_list")?;
        let agents = orchestrator
            .list_agent_types()
            .await
            .into_iter()
            .map(|d| AgentDescriptorDto {
                name: d.name,
                description: d.description,
                model: d.model,
            })
            .collect();
        to_result(&AgentListResult { agents })
    }

    async fn agent_cancel(&self, params: &Value) -> Result<Value, RpcError> {
        let orchestrator = self.orchestrator_for("agent_cancel")?;
        let p: AgentCancelParams = parse_params(params)?;
        // Foreign/unknown id -> a NotFound outcome (not an RPC error): cancel
        // is scoped to this plugin's own spawns.
        let known = self
            .agents
            .lock()
            .expect("agents map poisoned")
            .contains_key(&p.id);
        let outcome = if known {
            match orchestrator.cancel(&p.id).await {
                OrchestratorCancel::Cancelled => AgentCancelOutcomeDto::Cancelled,
                OrchestratorCancel::AlreadyFinished => AgentCancelOutcomeDto::AlreadyFinished,
                OrchestratorCancel::NotFound => AgentCancelOutcomeDto::NotFound,
            }
        } else {
            AgentCancelOutcomeDto::NotFound
        };
        to_result(&AgentCancelResult { outcome })
    }

    /// Serve a plugin notification (no reply). Only `log_emit` today.
    pub async fn handle_notification(&self, method: &str, params: &Value) {
        match method {
            "log_emit" => match serde_json::from_value::<LogEmitParams>(params.clone()) {
                Ok(p) => self.emit_log(p),
                Err(e) => tracing::warn!(plugin = %self.name, "malformed log_emit: {e}"),
            },
            other => {
                tracing::warn!(plugin = %self.name, "unknown plugin notification: {other}");
            }
        }
    }

    fn emit_log(&self, p: LogEmitParams) {
        let plugin = &self.name;
        let fields = p.fields.unwrap_or(Value::Null);
        // Forward at the plugin's chosen level, tagging the source plugin.
        match p.level {
            LogLevelDto::Debug => {
                tracing::debug!(plugin = %plugin, fields = %fields, "{}", p.message)
            }
            LogLevelDto::Info => {
                tracing::info!(plugin = %plugin, fields = %fields, "{}", p.message)
            }
            LogLevelDto::Warn => {
                tracing::warn!(plugin = %plugin, fields = %fields, "{}", p.message)
            }
            LogLevelDto::Error => {
                tracing::error!(plugin = %plugin, fields = %fields, "{}", p.message)
            }
        }
    }
}

/// Host-side state for one plugin-spawned subagent: the terminal outcome slot,
/// the capped event log, and a waker for wait/events long-polls.
#[derive(Default)]
struct AgentHandle {
    outcome: std::sync::Mutex<Option<AgentOutcome>>,
    /// Set exactly once, together with the terminal event push.
    terminal_pushed: AtomicBool,
    events: std::sync::Mutex<AgentEventLog>,
    last_progress: std::sync::Mutex<Option<AgentProgress>>,
    notify: tokio::sync::Notify,
}

#[derive(Default)]
struct AgentEventLog {
    next_seq: u64,
    entries: VecDeque<AgentEventDto>,
}

impl AgentHandle {
    fn outcome_snapshot(&self) -> Option<AgentOutcome> {
        self.outcome.lock().expect("outcome poisoned").clone()
    }

    /// Append an event (dropping the oldest past the cap) and wake pollers.
    fn push_event(&self, kind: AgentEventKindDto, data: Value) {
        {
            let mut log = self.events.lock().expect("event log poisoned");
            let seq = log.next_seq;
            log.next_seq += 1;
            log.entries.push_back(AgentEventDto { seq, kind, data });
            if log.entries.len() > AGENT_EVENT_BUFFER_CAP {
                log.entries.pop_front();
            }
        }
        self.notify.notify_waiters();
    }

    /// Events with `seq >= cursor`, plus the next cursor (one past the log).
    fn events_since(&self, cursor: u64) -> (Vec<AgentEventDto>, u64) {
        let log = self.events.lock().expect("event log poisoned");
        let events = log
            .entries
            .iter()
            .filter(|e| e.seq >= cursor)
            .cloned()
            .collect();
        (events, log.next_seq)
    }

    /// Record a progress snapshot; `true` when it differs from the previous
    /// one (i.e. a `progress` event should be emitted).
    fn record_progress(&self, progress: &AgentProgress) -> bool {
        let mut last = self.last_progress.lock().expect("progress poisoned");
        if last.as_ref() == Some(progress) {
            return false;
        }
        *last = Some(progress.clone());
        true
    }

    /// Store the terminal outcome (first writer wins), push the terminal
    /// event, and wake every waiter.
    fn complete(&self, outcome: AgentOutcome) {
        {
            let mut slot = self.outcome.lock().expect("outcome poisoned");
            if slot.is_some() {
                return;
            }
            *slot = Some(outcome.clone());
        }
        if !self.terminal_pushed.swap(true, Ordering::AcqRel) {
            let kind = match outcome.status {
                AgentStatusDto::Completed => AgentEventKindDto::Completed,
                AgentStatusDto::Cancelled => AgentEventKindDto::Cancelled,
                // `Running` can't be terminal; fold defensively into Failed.
                AgentStatusDto::Failed | AgentStatusDto::Running => AgentEventKindDto::Failed,
            };
            let data = serde_json::to_value(wait_result_of(&outcome)).unwrap_or(Value::Null);
            self.push_event(kind, data);
        }
        self.notify.notify_waiters();
    }
}

/// Map a terminal outcome onto the `agent_wait` wire shape.
fn wait_result_of(outcome: &AgentOutcome) -> AgentWaitResult {
    AgentWaitResult {
        status: outcome.status,
        output: Some(outcome.output.clone()),
        error: outcome.error.clone(),
        tokens_used: outcome.tokens_used,
        duration_ms: outcome.duration_ms,
        tool_calls: outcome.tool_calls,
        turns: outcome.turns,
    }
}

/// Await the spawn's terminal result (bounded by the per-spawn timeout, when
/// set) and complete the handle. On timeout the subagent is cancelled and the
/// watcher waits a short grace for the real (cancelled) result so its counters
/// still reach the plugin; a session teardown maps to a synthetic failure.
fn spawn_outcome_watcher(
    orchestrator: Arc<dyn AgentOrchestrator>,
    id: String,
    mut result_rx: tokio::sync::oneshot::Receiver<AgentOutcome>,
    handle: Arc<AgentHandle>,
    timeout_ms: Option<u64>,
) {
    const TORN_DOWN: &str = "session torn down before the subagent finished";
    tokio::spawn(async move {
        let outcome = match timeout_ms {
            None => match (&mut result_rx).await {
                Ok(outcome) => outcome,
                Err(_) => AgentOutcome::infra_failure(AgentStatusDto::Failed, TORN_DOWN),
            },
            Some(ms) => {
                match tokio::time::timeout(Duration::from_millis(ms), &mut result_rx).await {
                    Ok(Ok(outcome)) => outcome,
                    Ok(Err(_)) => AgentOutcome::infra_failure(AgentStatusDto::Failed, TORN_DOWN),
                    Err(_elapsed) => {
                        let _ = orchestrator.cancel(&id).await;
                        let timeout_note = format!("per-spawn timeout after {ms} ms; cancelled");
                        match tokio::time::timeout(AGENT_TIMEOUT_CANCEL_GRACE, &mut result_rx)
                            .await
                        {
                            // Keep the real counters, but the status is the
                            // timeout's: cancelled, with the budget recorded.
                            Ok(Ok(real)) => AgentOutcome {
                                status: AgentStatusDto::Cancelled,
                                error: Some(timeout_note),
                                ..real
                            },
                            _ => AgentOutcome::infra_failure(
                                AgentStatusDto::Cancelled,
                                timeout_note,
                            ),
                        }
                    }
                }
            }
        };
        handle.complete(outcome);
    });
}

fn parse_params<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, RpcError> {
    serde_json::from_value(params.clone()).map_err(|e| RpcError::invalid_params(e.to_string()))
}

fn to_result<T: serde::Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::internal(e.to_string()))
}

fn storage_err(e: std::io::Error) -> RpcError {
    RpcError::internal(format!("storage: {e}"))
}

/// A per-plugin JSON key/value store persisted to a single file.
///
/// The in-memory map is the source of truth once loaded; every mutation is
/// write-through with an atomic tmp-file + rename so a crash never leaves a
/// half-written store. The `Mutex` serializes all access for one plugin, which
/// is exactly the "core guarantees atomicity + locking" promise in the contract.
struct PluginStorage {
    path: PathBuf,
    /// `None` until first access; then the loaded map.
    inner: Mutex<Option<Map<String, Value>>>,
}

impl PluginStorage {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            inner: Mutex::new(None),
        }
    }

    /// Load the on-disk map into `slot` if not already loaded.
    async fn ensure_loaded<'a>(
        &self,
        slot: &'a mut Option<Map<String, Value>>,
    ) -> std::io::Result<&'a mut Map<String, Value>> {
        if slot.is_none() {
            let map = match tokio::fs::read(&self.path).await {
                Ok(bytes) => serde_json::from_slice::<Map<String, Value>>(&bytes)
                    .unwrap_or_else(|e| {
                        tracing::warn!(path = %self.path.display(), "corrupt storage, starting empty: {e}");
                        Map::new()
                    }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
                Err(e) => return Err(e),
            };
            *slot = Some(map);
        }
        Ok(slot.as_mut().expect("just loaded"))
    }

    async fn get(&self, key: &str) -> std::io::Result<Option<Value>> {
        let mut guard = self.inner.lock().await;
        let map = self.ensure_loaded(&mut guard).await?;
        Ok(map.get(key).cloned())
    }

    async fn set(&self, key: String, value: Value) -> std::io::Result<()> {
        let mut guard = self.inner.lock().await;
        let map = self.ensure_loaded(&mut guard).await?;
        map.insert(key, value);
        self.persist(map).await
    }

    async fn delete(&self, key: &str) -> std::io::Result<bool> {
        let mut guard = self.inner.lock().await;
        let map = self.ensure_loaded(&mut guard).await?;
        let existed = map.remove(key).is_some();
        if existed {
            self.persist(map).await?;
        }
        Ok(existed)
    }

    async fn list(&self, prefix: Option<&str>) -> std::io::Result<Vec<String>> {
        let mut guard = self.inner.lock().await;
        let map = self.ensure_loaded(&mut guard).await?;
        let mut keys: Vec<String> = map
            .keys()
            .filter(|k| prefix.is_none_or(|p| k.starts_with(p)))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }

    /// Atomically overwrite the store file: write a sibling temp file, fsync-free
    /// `persist` (rename) into place. The temp file lives in the same directory
    /// so the rename stays on one filesystem; a failure leaves the old file
    /// intact and cleans up the temp.
    async fn persist(&self, map: &Map<String, Value>) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(map)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let parent = self
            .path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        tokio::fs::create_dir_all(&parent).await?;
        let path = self.path.clone();
        // tempfile's API is blocking; keep it off the async reactor.
        tokio::task::spawn_blocking(move || {
            let mut tmp = tempfile::NamedTempFile::new_in(&parent)?;
            std::io::Write::write_all(&mut tmp, &bytes)?;
            tmp.persist(&path).map_err(|e| e.error)?;
            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(|e| std::io::Error::other(format!("storage persist task panicked: {e}")))?
    }
}

/// Build a plugin's storage path under `data_dir`, sanitizing the name into a
/// safe single filename.
pub fn storage_path(data_dir: &std::path::Path, plugin_name: &str) -> PathBuf {
    let safe: String = plugin_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    data_dir.join(format!("{safe}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn caps(dir: &std::path::Path, config: Value) -> PluginCapabilities {
        PluginCapabilities::new("my.plugin".into(), config, storage_path(dir, "my.plugin"))
    }

    #[tokio::test]
    async fn storage_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let c = caps(dir.path(), Value::Null);

        // get on empty -> null value.
        let r = c
            .handle_request("storage_get", &json!({"key": "a"}))
            .await
            .unwrap();
        assert_eq!(r, json!({ "value": null }));

        // set two keys.
        c.handle_request("storage_set", &json!({"key": "a", "value": 1}))
            .await
            .unwrap();
        c.handle_request("storage_set", &json!({"key": "b/x", "value": "v"}))
            .await
            .unwrap();

        // get returns the stored value.
        let r = c
            .handle_request("storage_get", &json!({"key": "a"}))
            .await
            .unwrap();
        assert_eq!(r, json!({ "value": 1 }));

        // list with and without prefix (sorted).
        let r = c.handle_request("storage_list", &json!({})).await.unwrap();
        assert_eq!(r, json!({ "keys": ["a", "b/x"] }));
        let r = c
            .handle_request("storage_list", &json!({"prefix": "b/"}))
            .await
            .unwrap();
        assert_eq!(r, json!({ "keys": ["b/x"] }));

        // delete reports existence, is idempotent.
        let r = c
            .handle_request("storage_delete", &json!({"key": "a"}))
            .await
            .unwrap();
        assert_eq!(r, json!({ "existed": true }));
        let r = c
            .handle_request("storage_delete", &json!({"key": "a"}))
            .await
            .unwrap();
        assert_eq!(r, json!({ "existed": false }));
    }

    #[tokio::test]
    async fn storage_persists_across_instances_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = caps(dir.path(), Value::Null);
            c.handle_request("storage_set", &json!({"key": "k", "value": {"n": 42}}))
                .await
                .unwrap();
        }
        // Fresh instance reads the persisted file.
        let c = caps(dir.path(), Value::Null);
        let r = c
            .handle_request("storage_get", &json!({"key": "k"}))
            .await
            .unwrap();
        assert_eq!(r, json!({ "value": { "n": 42 } }));

        // Atomicity: only the store file remains, no dangling temp files.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["my.plugin.json".to_string()]);
    }

    #[tokio::test]
    async fn config_get_returns_registered_config() {
        let dir = tempfile::tempdir().unwrap();
        let c = caps(dir.path(), json!({ "on": true }));
        let r = c.handle_request("config_get", &json!({})).await.unwrap();
        assert_eq!(r, json!({ "value": { "on": true } }));
    }

    /// Pre-wiring behavior is preserved: without an injected orchestrator,
    /// every `agent_*` method (and anything unknown) is `method_not_found`
    /// regardless of params, so plugins can feature-detect orchestration.
    #[tokio::test]
    async fn agent_methods_without_orchestrator_are_method_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let c = caps(dir.path(), Value::Null);
        for m in [
            "agent_spawn",
            "agent_wait",
            "agent_events",
            "agent_list",
            "agent_cancel",
            "agent_events_subscribe",
            "totally_unknown",
        ] {
            let err = c.handle_request(m, &json!({})).await.unwrap_err();
            assert_eq!(err.code, -32601, "method {m}");
        }
    }

    // ── agent_* orchestration over a mock orchestrator ──────────────────────

    use crate::orchestration::{
        AgentDescriptor, AgentOrchestrator, AgentOutcome, AgentProgress, AgentSpawnSpec,
        OrchestratorCancel, OrchestratorFuture, SpawnedSubagent,
    };
    use std::collections::HashMap;
    use xai_grok_plugin_protocol::AgentStatusDto;

    /// A scriptable in-memory orchestrator: spawns hand out ids + a oneshot
    /// the test completes; cancel resolves the pending oneshot with a
    /// cancelled outcome; progress serves a settable snapshot.
    #[derive(Default)]
    struct MockOrchestrator {
        next_id: std::sync::Mutex<u32>,
        pending: std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<AgentOutcome>>>,
        progress: std::sync::Mutex<Option<AgentProgress>>,
        cancelled: std::sync::Mutex<Vec<String>>,
        seen_specs: std::sync::Mutex<Vec<AgentSpawnSpec>>,
    }

    impl MockOrchestrator {
        fn complete(&self, id: &str, outcome: AgentOutcome) {
            let tx = self
                .pending
                .lock()
                .unwrap()
                .remove(id)
                .expect("no pending spawn for id");
            let _ = tx.send(outcome);
        }

        fn set_progress(&self, progress: Option<AgentProgress>) {
            *self.progress.lock().unwrap() = progress;
        }
    }

    impl AgentOrchestrator for MockOrchestrator {
        fn spawn(&self, spec: AgentSpawnSpec) -> Result<SpawnedSubagent, String> {
            let id = {
                let mut n = self.next_id.lock().unwrap();
                *n += 1;
                format!("agent-{n}")
            };
            self.seen_specs.lock().unwrap().push(spec);
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending.lock().unwrap().insert(id.clone(), tx);
            Ok(SpawnedSubagent { id, result_rx: rx })
        }

        fn progress<'a>(&'a self, _id: &'a str) -> OrchestratorFuture<'a, Option<AgentProgress>> {
            Box::pin(async move { self.progress.lock().unwrap().clone() })
        }

        fn cancel<'a>(&'a self, id: &'a str) -> OrchestratorFuture<'a, OrchestratorCancel> {
            Box::pin(async move {
                self.cancelled.lock().unwrap().push(id.to_string());
                match self.pending.lock().unwrap().remove(id) {
                    Some(tx) => {
                        let _ = tx.send(AgentOutcome {
                            status: AgentStatusDto::Cancelled,
                            output: String::new(),
                            error: None,
                            tokens_used: 5,
                            duration_ms: 50,
                            tool_calls: 1,
                            turns: 1,
                        });
                        OrchestratorCancel::Cancelled
                    }
                    None => OrchestratorCancel::AlreadyFinished,
                }
            })
        }

        fn list_agent_types<'a>(&'a self) -> OrchestratorFuture<'a, Vec<AgentDescriptor>> {
            Box::pin(async move {
                vec![
                    AgentDescriptor {
                        name: "Explore".to_string(),
                        description: "search the repo".to_string(),
                        model: Some("grok-code-fast-1".to_string()),
                    },
                    AgentDescriptor {
                        name: "general-purpose".to_string(),
                        description: "general tasks".to_string(),
                        model: None,
                    },
                ]
            })
        }
    }

    fn caps_with_orchestrator(
        dir: &std::path::Path,
    ) -> (PluginCapabilities, Arc<MockOrchestrator>) {
        let c = caps(dir, Value::Null);
        let orch = Arc::new(MockOrchestrator::default());
        c.set_orchestrator(Arc::clone(&orch) as Arc<dyn AgentOrchestrator>);
        (c, orch)
    }

    async fn spawn_one(c: &PluginCapabilities, params: Value) -> String {
        let r = c.handle_request("agent_spawn", &params).await.unwrap();
        r["id"].as_str().expect("spawn returns an id").to_string()
    }

    fn done_outcome() -> AgentOutcome {
        AgentOutcome {
            status: AgentStatusDto::Completed,
            output: "report text".into(),
            error: None,
            tokens_used: 1234,
            duration_ms: 900,
            tool_calls: 4,
            turns: 2,
        }
    }

    #[tokio::test]
    async fn agent_spawn_wait_and_events_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (c, orch) = caps_with_orchestrator(dir.path());

        let id = spawn_one(
            &c,
            json!({ "agent_type": "Explore", "prompt": "map the repo" }),
        )
        .await;
        assert_eq!(id, "agent-1");
        // The spec reached the orchestrator with the plugin attributed.
        {
            let specs = orch.seen_specs.lock().unwrap();
            assert_eq!(specs.len(), 1);
            assert_eq!(specs[0].plugin, "my.plugin");
            assert_eq!(specs[0].agent_type.as_deref(), Some("Explore"));
        }

        // Immediate events poll: the spawned event, not done.
        let r = c
            .handle_request("agent_events", &json!({ "id": id }))
            .await
            .unwrap();
        assert_eq!(r["events"][0]["seq"], 0);
        assert_eq!(r["events"][0]["kind"], "spawned");
        assert_eq!(r["done"], false);
        assert_eq!(r["next_cursor"], 1);

        // Wait with a short budget while still running -> status running.
        let r = c
            .handle_request("agent_wait", &json!({ "id": id, "timeout_ms": 30 }))
            .await
            .unwrap();
        assert_eq!(r["status"], "running");

        // Complete, then wait returns the terminal result inline.
        orch.complete(&id, done_outcome());
        let r = c
            .handle_request("agent_wait", &json!({ "id": id, "timeout_ms": 5000 }))
            .await
            .unwrap();
        assert_eq!(r["status"], "completed");
        assert_eq!(r["output"], "report text");
        assert_eq!(r["tokens_used"], 1234);
        assert_eq!(r["turns"], 2);

        // Events after the cursor: exactly the terminal event, done=true.
        let r = c
            .handle_request("agent_events", &json!({ "id": id, "cursor": 1 }))
            .await
            .unwrap();
        assert_eq!(r["events"].as_array().unwrap().len(), 1);
        assert_eq!(r["events"][0]["kind"], "completed");
        assert_eq!(r["events"][0]["data"]["output"], "report text");
        assert_eq!(r["done"], true);

        // Cancel after completion -> already_finished (pending map is empty).
        let r = c
            .handle_request("agent_cancel", &json!({ "id": id }))
            .await
            .unwrap();
        assert_eq!(r["outcome"], "already_finished");
    }

    #[tokio::test]
    async fn agent_events_emits_progress_on_change_only() {
        let dir = tempfile::tempdir().unwrap();
        let (c, orch) = caps_with_orchestrator(dir.path());
        let id = spawn_one(&c, json!({ "prompt": "work" })).await;

        orch.set_progress(Some(AgentProgress {
            phase: "running",
            turns: 1,
            tool_calls: 3,
            tokens_used: 42,
            elapsed_ms: 100,
        }));
        let r = c
            .handle_request("agent_events", &json!({ "id": id, "cursor": 1 }))
            .await
            .unwrap();
        assert_eq!(r["events"][0]["kind"], "progress");
        assert_eq!(r["events"][0]["data"]["tool_calls"], 3);
        let cursor = r["next_cursor"].as_u64().unwrap();

        // Unchanged progress -> no new event (immediate poll returns empty).
        let r = c
            .handle_request("agent_events", &json!({ "id": id, "cursor": cursor }))
            .await
            .unwrap();
        assert_eq!(r["events"].as_array().unwrap().len(), 0);
        assert_eq!(r["done"], false);
    }

    #[tokio::test]
    async fn agent_spawn_timeout_cancels_and_reports_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let (c, orch) = caps_with_orchestrator(dir.path());
        // 30 ms per-spawn budget; the mock never completes on its own, so the
        // watcher cancels it (the mock then resolves with a cancelled result).
        let id = spawn_one(&c, json!({ "prompt": "slow", "timeout_ms": 30 })).await;

        let r = c
            .handle_request("agent_wait", &json!({ "id": id, "timeout_ms": 5000 }))
            .await
            .unwrap();
        assert_eq!(r["status"], "cancelled");
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .contains("per-spawn timeout after 30 ms"),
            "error should mention the budget: {r}"
        );
        // Real counters from the cancelled child are preserved.
        assert_eq!(r["tokens_used"], 5);
        assert_eq!(
            orch.cancelled.lock().unwrap().as_slice(),
            std::slice::from_ref(&id)
        );

        // The terminal event is a `cancelled` event.
        let r = c
            .handle_request("agent_events", &json!({ "id": id, "cursor": 1 }))
            .await
            .unwrap();
        assert_eq!(r["events"][0]["kind"], "cancelled");
        assert_eq!(r["done"], true);
    }

    #[tokio::test]
    async fn agent_list_and_unknown_id_scoping() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _orch) = caps_with_orchestrator(dir.path());

        let r = c.handle_request("agent_list", &json!({})).await.unwrap();
        assert_eq!(
            r,
            json!({ "agents": [
                { "name": "Explore", "description": "search the repo", "model": "grok-code-fast-1" },
                { "name": "general-purpose", "description": "general tasks" },
            ] })
        );

        // wait/events on a foreign id -> invalid params; cancel -> not_found.
        for m in ["agent_wait", "agent_events"] {
            let err = c
                .handle_request(m, &json!({ "id": "not-mine" }))
                .await
                .unwrap_err();
            assert_eq!(err.code, -32602, "method {m}");
        }
        let r = c
            .handle_request("agent_cancel", &json!({ "id": "not-mine" }))
            .await
            .unwrap();
        assert_eq!(r["outcome"], "not_found");
    }

    /// The session dying (result sender dropped without a value) surfaces as a
    /// failed terminal result rather than a hang.
    #[tokio::test]
    async fn agent_wait_reports_failure_when_sender_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (c, orch) = caps_with_orchestrator(dir.path());
        let id = spawn_one(&c, json!({ "prompt": "doomed" })).await;
        drop(orch.pending.lock().unwrap().remove(&id));

        let r = c
            .handle_request("agent_wait", &json!({ "id": id, "timeout_ms": 5000 }))
            .await
            .unwrap();
        assert_eq!(r["status"], "failed");
        assert!(r["error"].as_str().unwrap().contains("torn down"));
    }

    /// A `Vec<u8>`-backed `MakeWriter` so we can assert on formatted log output.
    #[derive(Clone)]
    struct CaptureWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// `log_emit` reaches `tracing` at the requested level, tagged with the
    /// plugin name and structured fields.
    #[tokio::test]
    async fn log_emit_forwards_to_tracing() {
        let dir = tempfile::tempdir().unwrap();
        let c = caps(dir.path(), Value::Null);

        let buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CaptureWriter(Arc::clone(&buf)))
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        c.handle_notification(
            "log_emit",
            &json!({ "level": "warn", "message": "hello from plugin", "fields": { "k": 1 } }),
        )
        .await;
        drop(_guard);

        let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains("hello from plugin"),
            "message missing: {text}"
        );
        assert!(text.contains("WARN"), "level missing: {text}");
        assert!(text.contains("my.plugin"), "plugin name missing: {text}");
        assert!(text.contains("plugin"), "plugin field missing: {text}");
    }
}
