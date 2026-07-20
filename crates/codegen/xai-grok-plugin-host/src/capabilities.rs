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
//! - `agent_*`      → reserved orchestration surface; replies `method_not_found`.
//!
//! One [`PluginCapabilities`] is built per registered plugin and shared with
//! every sidecar spawned for it (across restarts), so storage state survives a
//! crash-restart.

use std::path::PathBuf;

use serde_json::{Map, Value};
use tokio::sync::Mutex;
use xai_grok_plugin_protocol::{
    ConfigGetResult, LogEmitParams, LogLevelDto, StorageDeleteParams, StorageDeleteResult,
    StorageGetParams, StorageGetResult, StorageListParams, StorageListResult, StorageSetParams,
    StorageSetResult,
};

use crate::rpc::RpcError;

/// Per-plugin capability context. Cheap to `Arc`-clone into each sidecar.
pub struct PluginCapabilities {
    name: String,
    config: Value,
    storage: PluginStorage,
}

impl PluginCapabilities {
    pub fn new(name: String, config: Value, storage_path: PathBuf) -> Self {
        Self {
            name,
            config,
            storage: PluginStorage::new(storage_path),
        }
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
            // Reserved orchestration surface: named in the v1 dictionary
            // but not wired until the subagent-coordinator seam lands.
            "agent_spawn"
            | "agent_events_subscribe"
            | "agent_wait"
            | "agent_cancel"
            | "agent_list" => Err(RpcError::method_not_found(method)),
            other => Err(RpcError::method_not_found(other)),
        }
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

    #[tokio::test]
    async fn agent_and_unknown_methods_are_method_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let c = caps(dir.path(), Value::Null);
        for m in ["agent_spawn", "agent_wait", "agent_list", "totally_unknown"] {
            let err = c.handle_request(m, &json!({})).await.unwrap_err();
            assert_eq!(err.code, -32601, "method {m}");
        }
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
