//! A single plugin sidecar: the spawned child plus the bidirectional JSON-RPC
//! loop over its stdio.
//!
//! Structure mirrors the codebase's request/response idiom (an mpsc writer task
//! plus a pending-request map keyed by id — see `xai-acp-lib`'s gateway), adapted
//! to a plain multi-threaded `tokio::spawn` world (no `LocalSet`/`?Send`):
//!
//! - a **writer task** owns the child's stdin and drains an `mpsc` of compact
//!   JSON lines (our requests, our replies to the plugin, notifications);
//! - a **reader task** owns stdout, decodes each `\n`-delimited frame, and either
//!   completes a pending request (response), or dispatches an inbound
//!   request/notification to the [`PluginCapabilities`] server;
//! - a **stderr task** copies the child's stderr to `tracing` with a `plugin`
//!   prefix.
//!
//! Framing is a local capped read-line loop rather than a dependency on
//! `xai-acp-lib::LineBufferedRead`: that type exists to make ACP's
//! `select_biased!` `read_line` cancel-safe (a `poll_read` shim over
//! `spawn_local`), which is a poor fit for a plain reader task that just wants
//! "the next line". A tokio `fill_buf`/`consume` loop with the same 64 MiB cap is
//! simpler and Send. The cap is generous by design — the plugin channel is
//! explicitly exempt from the 128 KiB command-hook `MAX_PAYLOAD_SIZE`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use xai_grok_plugin_protocol::{
    InitializeParams, InitializeResult, PROTOCOL_VERSION, ShutdownParams,
};
use xai_tty_utils::ProcessGroup;

use crate::capabilities::PluginCapabilities;
use crate::rpc::{self, Inbound, RpcError};

/// Max size of a single NDJSON frame (64 MiB), matching `xai-acp-lib`. Bounds
/// memory if the plugin never emits a newline.
const MAX_LINE_SIZE: usize = 64 * 1024 * 1024;

/// Grace period a plugin gets to exit after `shutdown` before SIGKILL.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// The outcome delivered to a pending request: the plugin's `result`, or a
/// JSON-RPC `error`.
type RpcOutcome = Result<Value, RpcError>;

/// A single RPC call failure.
#[derive(Debug)]
pub enum SidecarError {
    /// The transport is gone — the child died or stdio closed.
    Closed,
    /// The plugin didn't reply within the deadline (it may still be alive).
    Timeout,
    /// The plugin replied with a JSON-RPC error.
    Rpc(RpcError),
}

/// Why a sidecar failed to come up. Drives supervisor policy: `VersionMismatch`
/// disables permanently; the rest count as a crash and back off.
#[derive(Debug)]
pub enum StartError {
    /// The process couldn't be launched (runtime discovery or `exec` failed).
    Spawn(String),
    /// `initialize` failed (timeout, closed pipe, RPC error, or bad reply).
    Handshake(String),
    /// The plugin's `protocol_version` doesn't match the host's.
    VersionMismatch { plugin: u32, host: u32 },
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Spawn(m) => write!(f, "spawn failed: {m}"),
            StartError::Handshake(m) => write!(f, "handshake failed: {m}"),
            StartError::VersionMismatch { plugin, host } => {
                write!(f, "protocol version mismatch: plugin={plugin} host={host}")
            }
        }
    }
}

/// A live sidecar. Shared behind an `Arc` by the supervisor; all methods take
/// `&self` so concurrent invokes don't serialize on it.
pub struct PluginSidecar {
    name: String,
    writer_tx: mpsc::UnboundedSender<String>,
    next_id: AtomicI64,
    pending: Arc<Mutex<std::collections::HashMap<i64, oneshot::Sender<RpcOutcome>>>>,
    alive: Arc<AtomicBool>,
    /// Child + group behind std mutexes so `Drop` (sync) and `dispose` (async)
    /// can both reach them without holding a guard across an await.
    child: Mutex<Option<tokio::process::Child>>,
    group: Mutex<Option<ProcessGroup>>,
}

impl PluginSidecar {
    /// Spawn `cmd` as a sidecar and wire up its IO loop. Does **not** handshake;
    /// call [`Self::handshake`] next.
    ///
    /// `caps` serves inbound plugin→core traffic and is shared across restarts.
    pub fn spawn(
        mut cmd: Command,
        name: String,
        caps: Arc<PluginCapabilities>,
    ) -> std::io::Result<Arc<Self>> {
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Detach into its own session/group so teardown can killpg grandchildren
        // (e.g. bun/node spawning workers) without orphaning them.
        xai_tty_utils::detach_command(&mut cmd);

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("plugin stdin missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("plugin stdout missing"))?;
        let stderr = child.stderr.take();

        // Best-effort process group; a failure just degrades to leader-only kill.
        let group = match ProcessGroup::new() {
            Ok(mut g) => match g.attach(&child) {
                Ok(()) => Some(g),
                Err(e) => {
                    tracing::warn!(plugin = %name, "process group attach failed: {e}");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(plugin = %name, "process group create failed: {e}");
                None
            }
        };

        let (writer_tx, writer_rx) = mpsc::unbounded_channel::<String>();
        let pending: Arc<Mutex<std::collections::HashMap<i64, oneshot::Sender<RpcOutcome>>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));

        spawn_writer_task(name.clone(), stdin, writer_rx, Arc::clone(&alive));
        spawn_reader_task(
            name.clone(),
            stdout,
            Arc::clone(&pending),
            Arc::clone(&alive),
            caps,
            writer_tx.clone(),
        );
        if let Some(stderr) = stderr {
            spawn_stderr_task(name.clone(), stderr);
        }

        Ok(Arc::new(Self {
            name,
            writer_tx,
            next_id: AtomicI64::new(1),
            pending,
            alive,
            child: Mutex::new(Some(child)),
            group: Mutex::new(group),
        }))
    }

    /// Whether the transport is still up (IO tasks running, child not reaped).
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// Issue a request and await its reply, bounded by `timeout`.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, SidecarError> {
        if !self.is_alive() {
            return Err(SidecarError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending map poisoned")
            .insert(id, tx);

        let frame = rpc::request_frame(id, method, &params);
        if self.writer_tx.send(frame).is_err() {
            self.pending
                .lock()
                .expect("pending map poisoned")
                .remove(&id);
            return Err(SidecarError::Closed);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(rpc_err))) => Err(SidecarError::Rpc(rpc_err)),
            // Sender dropped: the reader task drained pending on transport close.
            Ok(Err(_recv)) => Err(SidecarError::Closed),
            Err(_elapsed) => {
                self.pending
                    .lock()
                    .expect("pending map poisoned")
                    .remove(&id);
                Err(SidecarError::Timeout)
            }
        }
    }

    /// Send a fire-and-forget notification (no reply expected).
    fn notify(&self, method: &str, params: Value) {
        let _ = self
            .writer_tx
            .send(rpc::notification_frame(method, &params));
    }

    /// Run the `initialize` handshake and validate the protocol version.
    pub async fn handshake(
        &self,
        params: InitializeParams,
        timeout: Duration,
    ) -> Result<InitializeResult, StartError> {
        let params_json = serde_json::to_value(&params)
            .map_err(|e| StartError::Handshake(format!("encode initialize params: {e}")))?;
        let value = match self.call("initialize", params_json, timeout).await {
            Ok(v) => v,
            Err(SidecarError::Timeout) => {
                return Err(StartError::Handshake("initialize timed out".into()));
            }
            Err(SidecarError::Closed) => {
                return Err(StartError::Handshake(
                    "transport closed during initialize".into(),
                ));
            }
            Err(SidecarError::Rpc(e)) => return Err(StartError::Handshake(e.to_string())),
        };
        let result: InitializeResult = serde_json::from_value(value)
            .map_err(|e| StartError::Handshake(format!("bad initialize reply: {e}")))?;
        if result.protocol_version != PROTOCOL_VERSION {
            return Err(StartError::VersionMismatch {
                plugin: result.protocol_version,
                host: PROTOCOL_VERSION,
            });
        }
        Ok(result)
    }

    /// Notify `shutdown`, wait up to [`SHUTDOWN_GRACE`], then SIGKILL. Idempotent.
    pub async fn dispose(&self, reason: &str) {
        self.alive.store(false, Ordering::Release);
        self.notify(
            "shutdown",
            serde_json::to_value(ShutdownParams {
                reason: reason.to_string(),
            })
            .unwrap_or(Value::Null),
        );

        let child = self.child.lock().expect("child lock poisoned").take();
        let Some(mut child) = child else { return };

        let killed_grandchildren = || {
            if let Some(g) = self.group.lock().expect("group lock poisoned").as_ref()
                && let Err(e) = g.kill()
            {
                tracing::warn!(plugin = %self.name, "killpg during dispose failed: {e}");
            }
        };

        tokio::select! {
            _ = tokio::time::sleep(SHUTDOWN_GRACE) => {
                killed_grandchildren();
                if let Err(e) = child.kill().await {
                    tracing::warn!(plugin = %self.name, "SIGKILL after grace failed: {e}");
                }
            }
            res = child.wait() => {
                killed_grandchildren();
                match res {
                    Ok(status) => tracing::debug!(plugin = %self.name, "sidecar exited: {status}"),
                    Err(e) => tracing::warn!(plugin = %self.name, "wait on sidecar failed: {e}"),
                }
            }
        }
        // Leader is reaped; drop the group so `Drop` can't killpg a reused pid.
        *self.group.lock().expect("group lock poisoned") = None;
    }
}

impl Drop for PluginSidecar {
    fn drop(&mut self) {
        // Synchronous group teardown first (safe from Drop).
        if let Some(g) = self.group.lock().expect("group lock poisoned").take()
            && let Err(e) = g.kill()
        {
            tracing::trace!(plugin = %self.name, "killpg on drop: {e}");
        }
        let Some(mut child) = self.child.lock().expect("child lock poisoned").take() else {
            return;
        };
        // kill_on_drop(true) is the backstop; be explicit so grandchildren-less
        // leaders die promptly even without an entered runtime.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = child.kill().await;
            });
        } else if let Err(e) = child.start_kill() {
            tracing::trace!(plugin = %self.name, "start_kill on drop: {e}");
        }
    }
}

/// Drain `writer_rx` to the child's stdin, one compact line each.
fn spawn_writer_task(
    name: String,
    mut stdin: tokio::process::ChildStdin,
    mut writer_rx: mpsc::UnboundedReceiver<String>,
    alive: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        while let Some(line) = writer_rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err()
                || stdin.write_all(b"\n").await.is_err()
                || stdin.flush().await.is_err()
            {
                tracing::debug!(plugin = %name, "sidecar stdin closed; writer exiting");
                alive.store(false, Ordering::Release);
                return;
            }
        }
    });
}

/// Decode frames from the child's stdout and route them.
fn spawn_reader_task(
    name: String,
    stdout: ChildStdout,
    pending: Arc<Mutex<std::collections::HashMap<i64, oneshot::Sender<RpcOutcome>>>>,
    alive: Arc<AtomicBool>,
    caps: Arc<PluginCapabilities>,
    writer_tx: mpsc::UnboundedSender<String>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            match read_line_capped(&mut reader, &mut buf).await {
                Ok(0) => break, // EOF: child closed stdout.
                Ok(_) => {
                    let value: Value = match serde_json::from_slice(&buf) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(plugin = %name, "undecodable frame skipped: {e}");
                            continue;
                        }
                    };
                    match rpc::classify(value) {
                        Some(Inbound::Response { id, result }) => {
                            if let Some(tx) =
                                pending.lock().expect("pending map poisoned").remove(&id)
                            {
                                let _ = tx.send(result);
                            } else {
                                tracing::debug!(plugin = %name, id, "response for unknown id");
                            }
                        }
                        Some(Inbound::Request { id, method, params }) => {
                            let caps = Arc::clone(&caps);
                            let writer_tx = writer_tx.clone();
                            let name = name.clone();
                            tokio::spawn(async move {
                                let reply = match caps.handle_request(&method, &params).await {
                                    Ok(result) => rpc::response_ok_frame(&id, result),
                                    Err(err) => rpc::response_err_frame(&id, &err),
                                };
                                if writer_tx.send(reply).is_err() {
                                    tracing::debug!(plugin = %name, "reply dropped; writer gone");
                                }
                            });
                        }
                        Some(Inbound::Notification { method, params }) => {
                            let caps = Arc::clone(&caps);
                            tokio::spawn(async move {
                                caps.handle_notification(&method, &params).await;
                            });
                        }
                        None => {
                            tracing::warn!(plugin = %name, "unroutable frame skipped");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(plugin = %name, "sidecar read error: {e}");
                    break;
                }
            }
        }
        // Transport is down: mark dead and fail every in-flight request so
        // callers get `Closed` instead of hanging until their timeout.
        alive.store(false, Ordering::Release);
        let drained: Vec<_> = pending
            .lock()
            .expect("pending map poisoned")
            .drain()
            .collect();
        drop(drained); // dropping the senders resolves each `rx` with RecvError.
    });
}

/// Copy the child's stderr to `tracing`, one line per record, with a `plugin`
/// prefix. Plugins log diagnostics here; keep it visible but out of stdout's
/// JSON-RPC stream.
fn spawn_stderr_task(name: String, stderr: ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => tracing::info!(plugin = %name, "[stderr] {line}"),
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!(plugin = %name, "stderr read error: {e}");
                    break;
                }
            }
        }
    });
}

/// Read one `\n`-delimited line into `buf`, capped at [`MAX_LINE_SIZE`].
///
/// Mirrors `xai-acp-lib::line_reader::read_line_capped`: checks the size after
/// each buffer fill so memory stays bounded even without a newline. Returns the
/// byte count (0 at EOF).
async fn read_line_capped(
    reader: &mut BufReader<ChildStdout>,
    buf: &mut Vec<u8>,
) -> std::io::Result<usize> {
    buf.clear();
    loop {
        let (consumed, done) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                return Ok(buf.len()); // EOF
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    buf.extend_from_slice(&available[..=pos]);
                    (pos + 1, true)
                }
                None => {
                    buf.extend_from_slice(available);
                    (available.len(), false)
                }
            }
        };
        reader.consume(consumed);
        if buf.len() > MAX_LINE_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("plugin frame exceeds {MAX_LINE_SIZE} bytes"),
            ));
        }
        if done {
            return Ok(buf.len());
        }
    }
}

/// Build the `initialize` params for a plugin. Kept here so the supervisor and
/// tests share one construction.
pub fn initialize_params(
    plugin_name: String,
    plugin_config: Value,
    workspace_root: PathBuf,
    session_id: String,
    storage: bool,
    leader_socket: Option<String>,
) -> InitializeParams {
    InitializeParams {
        protocol_version: PROTOCOL_VERSION,
        plugin_name,
        plugin_config,
        workspace_root: workspace_root.to_string_lossy().into_owned(),
        session_id,
        capabilities: xai_grok_plugin_protocol::HostCapabilities {
            storage,
            // Tier 1 orchestration: the leader's Unix-socket path, when the
            // hosting process is a leader (also exported to the sidecar env
            // as `GROK_LEADER_SOCKET`; see `runtime::build_command`).
            leader_socket,
        },
    }
}
