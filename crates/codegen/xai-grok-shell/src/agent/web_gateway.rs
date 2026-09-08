//! WebSocket gateway from browser clients onto the shared leader.
//!
//! Two halves of this already existed and neither was sufficient alone.
//!
//! [`crate::agent::server`] (`grok agent serve`) terminates a browser-shaped WebSocket with real
//! authentication, but it owns its own [`crate::agent::MvpAgent`] and its relay destination is a
//! single slot overwritten on every connection (`server.rs`, `RelayDest`), so a second browser
//! starves the first.
//!
//! [`crate::leader`] is genuinely N-client — replay on attach, live fan-out to every subscriber,
//! permission modals shared and answered first-come, subagents inheriting the parent's subscriber
//! set — but `ClientMessage::Register` carries a free-form `client_type` and no credential at all.
//!
//! So the multiplexing lives in one process and the authentication in another, and this module is
//! the bridge. It terminates and authenticates the WebSocket with the same [`validate_auth`] the
//! agent server uses, then opens **one leader registration per browser connection**. Each tab is
//! an ordinary leader client, which is what buys the whole tested session model for free: nothing
//! here re-implements replay, fan-out or permission arbitration.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{
        ConnectInfo, Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent::server::{WsQueryParams, validate_auth};
use crate::leader::protocol::ShutdownReason;
use crate::leader::{
    ClientCapabilities, ClientMode, LeaderClient, LeaderConnection, LeaderEnvUrls, connect_or_spawn,
};

/// IPC registration `client_type` for every browser connection.
///
/// Not cosmetic. The leader forwards it as `_meta.clientIdentifier`, where the agent maps it to
/// `ClientType::GrokWeb` (`xai-grok-workspace/src/permission/types.rs`, `from_client_identifier`),
/// which is a client allowed to present permission prompts. It is also what makes
/// `inject_session_request_context` do anything: that function early-returns when every capability
/// is false **and** `client_type` is empty, and its injection is what keeps this client's
/// terminal/fs settings per-client instead of falling back to shared agent state.
pub const WEB_GATEWAY_CLIENT_TYPE: &str = "grok-web";

/// Route the gateway listens on, matching `grok agent serve`.
const WS_ROUTE: &str = "/ws";

/// Same cadence as the agent server's WebSocket keepalive.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Frames queued for one browser before the gateway gives up on it.
///
/// The leader never blocks on a slow client — it hands each client an unbounded `kanal` queue and
/// pushes with `try_send` (`leader/server.rs`) — so a stalled browser cannot stall the leader's
/// event loop or the other clients. What it *can* do is grow that queue, and the unbounded mpsc
/// inside [`LeaderClient`], without limit. This bound converts that leak into a disconnect: a
/// browser that falls this far behind is dropped, and reconnects with a `session/load` that replays
/// the transcript. Losing frames silently would be worse than losing the socket.
const OUTBOUND_QUEUE_DEPTH: usize = 1024;

/// How long teardown waits for the writer to flush its close frame before abandoning the socket.
const WRITER_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Configuration for the gateway listener.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Address to bind. Loopback by default at the CLI, matching `grok agent serve`:
    /// the leader has no authentication of its own and must not become reachable off-box.
    pub bind_addr: SocketAddr,
    /// Shared secret required on every connection (`Authorization: Bearer` or `?server-key=`).
    pub secret: String,
}

/// One browser connection's registration on the leader, reduced to what the relay needs.
///
/// Built by [`Self::from_connection`] in production and [`Self::from_client`] where the socket is
/// already known (tests, and callers that resolved a leader themselves).
pub struct LeaderAttachment {
    /// ACP payloads from the browser to the leader.
    to_leader: mpsc::UnboundedSender<String>,
    /// ACP payloads from the leader to the browser.
    from_leader: mpsc::UnboundedReceiver<String>,
    /// Latest `ServerMessage::ShuttingDown` reason, if the leader announced one.
    /// The leader client consumes those frames itself; they never reach the browser as ACP.
    shutting_down: watch::Receiver<Option<ShutdownReason>>,
    /// Teardown for the registration, for closing it while the attachment is still held.
    ///
    /// Dropping `to_leader` closes the registration on its own (`leader/client.rs`), so this is no
    /// longer what keeps a departed browser from outliving itself. It still buys promptness:
    /// cancelling makes the write task send `ClientMessage::Disconnect` and exit right away, which
    /// lets the leader drop the subscriber, hand the session driver to another client, or evict the
    /// session when nobody is left (`leader/server.rs`, `ServerEvent::Disconnected`).
    cancel: CancellationToken,
}

impl LeaderAttachment {
    /// Wrap a connection obtained through leader discovery (`connect_or_spawn`).
    pub fn from_connection(conn: LeaderConnection) -> Self {
        let cancel = conn.cancel_token();
        let shutting_down = conn.shutting_down_reason();
        let (to_leader, from_leader) = conn.into_channels();
        Self {
            to_leader,
            from_leader,
            shutting_down,
            cancel,
        }
    }

    /// Wrap a client connected straight to a known leader socket.
    pub fn from_client(client: LeaderClient) -> Self {
        let cancel = client.cancel_token();
        let shutting_down = client.shutting_down_reason();
        let (to_leader, from_leader) = client.into_channels();
        Self {
            to_leader,
            from_leader,
            shutting_down,
            cancel,
        }
    }

    fn close(&self) {
        self.cancel.cancel();
    }
}

/// Opens one leader registration per browser connection.
pub type LeaderAttach = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<LeaderAttachment>> + Send>>
        + Send
        + Sync,
>;

/// Capabilities every browser connection registers with.
///
/// `terminal`, `fs_read` and `fs_write` are **false on purpose**. The doc comment on
/// `ClientCapabilities::terminal` (`leader/protocol.rs`) uses a web client with `terminal: true` as
/// its example, but that describes a client that can host a PTY itself and answer the agent's
/// terminal ACP calls. A browser acting as a thin view onto a local machine cannot do that, and
/// neither can this gateway: it moves JSON, it does not run processes or touch files. Turning any
/// of these on routes terminal creation and file reads/writes out to a client that will not answer
/// them. Terminals and file operations stay on the agent side.
///
/// `code_nav_enabled` and `status_line` are false for the same reason: nothing here renders them.
///
/// `interactive_trust` is `Some(true)`: a browser draws and answers the folder-trust card. `sdk/web` implements
/// `x.ai/folder_trust/request` and returns a bare `{"outcome": …}` — no `ExtMethodResult` envelope, which the agent's
/// `serde_json::from_str::<FolderTrustResponse>` would not decode — with dismissal sent back as a JSON-RPC error.
/// Without this, a browser session on any root that is not the launch dir resolves untrusted with no card and no
/// notice, silently dropping that project's MCP servers, hooks, plugins, LSP and permission rules.
///
/// This is a claim made for **every** authenticated WebSocket client, not only `sdk/web`, and the gateway cannot check
/// it: the leader registration is opened before the browser has sent a frame, and `ClientMessage` (`leader/protocol.rs`)
/// has no way to revise capabilities afterwards, so there is nothing to derive the answer from. It is safe to claim
/// anyway because every non-answer fails closed in `mvp_agent/folder_trust_prompt.rs`: an error reply (what an ACP peer
/// without the card returns for an unknown method) and an undecodable one leave the workspace gated and release the
/// dedup key, and silence hits `TRUST_PROMPT_TIMEOUT` with the workspace still gated. The session was created gated and
/// the round-trip runs detached, so nothing waits on it. Only an explicit `"trust"` unblocks; a client that ignores the
/// request ends up exactly where `Some(false)` left it.
///
/// `None` would not be that client-neutral middle ground. It means "did not say", which sends the agent back to its
/// shared, last-initialize-wins flag — and a browser's `initialize` declares no `x.ai/folderTrust` meta, so a browser
/// session's card would be decided by whichever client initialized last, a TUI sharing this leader included. Declaring
/// it in the browser's `initialize` instead would write that same shared flag and hand this `true` to every client that
/// registered `None`, which is the cross-client leak the per-client capability exists to close. The honest per-client
/// answer has to come from the registration.
fn browser_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        terminal: false,
        fs_read: false,
        fs_write: false,
        code_nav_enabled: false,
        status_line: false,
        interactive_trust: Some(true),
        client_version: Some(xai_grok_version::VERSION.to_string()),
        ..ClientCapabilities::default()
    }
}

/// Attach each browser through ordinary leader discovery, spawning a leader if none is running.
///
/// This is the same call every `grok` client makes, so a browser joins whichever leader the TUI is
/// already talking to, honours `GROK_LEADER_SOCKET`, and participates in the usual version-skew
/// eviction.
pub fn attach_via_discovery(env_urls: LeaderEnvUrls) -> LeaderAttach {
    Arc::new(move || {
        let env_urls = env_urls.clone();
        Box::pin(async move {
            // `ClientMode::Stdio`, never `Headless`: a headless registration flips the leader's
            // `relay_demand_tx` and makes it dial the grok.com relay (`leader/server.rs`). A browser
            // on loopback drives ACP through this socket and must not open an outbound relay.
            let conn = connect_or_spawn(
                WEB_GATEWAY_CLIENT_TYPE,
                ClientMode::Stdio,
                &env_urls,
                browser_capabilities(),
            )
            .await?;
            Ok(LeaderAttachment::from_connection(conn))
        })
    })
}

/// Attach each browser to the leader already listening at `socket_path`, skipping discovery.
///
/// For callers that resolved a leader themselves and must not spawn or evict one.
pub fn attach_at_socket(socket_path: PathBuf) -> LeaderAttach {
    Arc::new(move || {
        let socket_path = socket_path.clone();
        Box::pin(async move {
            let client = LeaderClient::connect(
                socket_path,
                WEB_GATEWAY_CLIENT_TYPE,
                ClientMode::Stdio,
                browser_capabilities(),
            )
            .await?;
            Ok(LeaderAttachment::from_client(client))
        })
    })
}

struct GatewayState {
    secret: String,
    attach: LeaderAttach,
}

/// Bind and serve the gateway until the listener fails.
pub async fn run_web_gateway(config: GatewayConfig, attach: LeaderAttach) -> anyhow::Result<()> {
    let listener = TcpListener::bind(config.bind_addr).await?;
    info!(
        "Web gateway listening on ws://{}{WS_ROUTE}",
        listener.local_addr().unwrap_or(config.bind_addr)
    );
    serve_web_gateway(listener, config.secret, attach).await
}

/// Serve the gateway on an already-bound listener.
///
/// Split out so a caller that needs the resolved port (bind to `:0`) can read `local_addr()` first.
pub async fn serve_web_gateway(
    listener: TcpListener,
    secret: String,
    attach: LeaderAttach,
) -> anyhow::Result<()> {
    let state = Arc::new(GatewayState { secret, attach });
    let app = Router::new()
        .route(WS_ROUTE, get(ws_handler))
        .with_state(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// WebSocket upgrade handler; authentication is identical to `grok agent serve`.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<GatewayState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<WsQueryParams>,
) -> Response {
    if !validate_auth(&headers, &query, &state.secret) {
        warn!("Web gateway: unauthorized connection attempt from {addr}");
        return (
            StatusCode::UNAUTHORIZED,
            "Invalid or missing authorization token",
        )
            .into_response();
    }
    info!("Web gateway: authenticated WebSocket connection from {addr}");
    ws.on_upgrade(move |socket| handle_connection(socket, state, addr))
}

/// Why one browser's relay ended. Determines the WebSocket close frame the browser sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayEnd {
    /// The browser closed the socket, or reading it failed.
    ClientGone,
    /// The leader connection ended. `shutting_down` distinguishes a planned shutdown.
    LeaderGone,
    /// The browser fell more than [`OUTBOUND_QUEUE_DEPTH`] frames behind.
    Backpressure,
    /// The writer half died (socket write error).
    WriteFailed,
}

async fn handle_connection(ws: WebSocket, state: Arc<GatewayState>, peer: SocketAddr) {
    let mut attachment = match (state.attach)().await {
        Ok(attachment) => attachment,
        Err(e) => {
            warn!(error = %e, "Web gateway: leader unavailable for {peer}");
            let (mut ws_write, _) = ws.split();
            // Do not start the relay: the browser would see a live socket that reaches no agent.
            let _ = ws_write
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::AGAIN,
                    reason: "leader unavailable".into(),
                })))
                .await;
            return;
        }
    };

    let (mut ws_write, mut ws_read) = ws.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_DEPTH);
    // Fires when the writer stops, so the relay loop below notices a dead socket even while idle.
    let writer_stopped = CancellationToken::new();
    let writer_stopped_guard = writer_stopped.clone().drop_guard();

    let mut writer = tokio::spawn(async move {
        let _guard = writer_stopped_guard;
        let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
        keepalive.tick().await;
        loop {
            tokio::select! {
                queued = out_rx.recv() => {
                    let Some(msg) = queued else { break };
                    let closing = matches!(msg, Message::Close(_));
                    if ws_write.send(msg).await.is_err() || closing {
                        break;
                    }
                }
                _ = keepalive.tick() => {
                    if ws_write.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let end = relay(
        &mut ws_read,
        &mut attachment,
        &out_tx,
        &writer_stopped,
        peer,
    )
    .await;

    // Close the leader registration before anything else. A browser that vanishes mid-turn must not
    // leave a subscriber behind: the leader reassigns the session driver to another subscriber, or
    // evicts the session when this was the last one.
    attachment.close();

    let close = close_frame_for(end, &attachment.shutting_down);
    let _ = out_tx.try_send(Message::Close(Some(close)));
    drop(out_tx);
    // Bounded, then abandoned. The writer parks in `ws_write.send().await`, and the connections most
    // in need of teardown are exactly the ones that stopped reading, so waiting for it to drain
    // would leak a task per dead browser. Aborting drops the socket, which the peer sees as a FIN.
    if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, &mut writer)
        .await
        .is_err()
    {
        debug!("Web gateway: {peer} did not drain in time; dropping the socket");
        writer.abort();
    }
    info!(?end, "Web gateway: connection ended for {peer}");
}

/// Pump ACP frames both ways until either side ends.
async fn relay(
    ws_read: &mut futures::stream::SplitStream<WebSocket>,
    attachment: &mut LeaderAttachment,
    out_tx: &mpsc::Sender<Message>,
    writer_stopped: &CancellationToken,
    peer: SocketAddr,
) -> RelayEnd {
    loop {
        tokio::select! {
            biased;
            _ = writer_stopped.cancelled() => return RelayEnd::WriteFailed,
            incoming = ws_read.next() => {
                let Some(frame) = incoming else {
                    return RelayEnd::ClientGone;
                };
                match frame {
                    Ok(Message::Text(text)) => {
                        let text: &str = text.as_ref();
                        if !forward_to_leader(text, &attachment.to_leader) {
                            return RelayEnd::LeaderGone;
                        }
                    }
                    Ok(Message::Binary(bin)) => {
                        let Ok(text) = std::str::from_utf8(&bin) else {
                            warn!("Web gateway: dropping non-UTF-8 binary frame from {peer}");
                            continue;
                        };
                        if !forward_to_leader(text, &attachment.to_leader) {
                            return RelayEnd::LeaderGone;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        if let Some(f) = frame {
                            debug!("Web gateway: close from {peer}: {} {}", f.code, f.reason);
                        }
                        return RelayEnd::ClientGone;
                    }
                    Ok(Message::Ping(_) | Message::Pong(_)) => {}
                    Err(e) => {
                        warn!(error = ?e, "Web gateway: read error from {peer}");
                        return RelayEnd::ClientGone;
                    }
                }
            }
            outgoing = attachment.from_leader.recv() => {
                let Some(payload) = outgoing else {
                    return RelayEnd::LeaderGone;
                };
                if let Some(end) = queue_for_browser(out_tx, payload) {
                    if end == RelayEnd::Backpressure {
                        warn!(
                            depth = OUTBOUND_QUEUE_DEPTH,
                            "Web gateway: {peer} is too far behind; dropping the connection"
                        );
                    }
                    return end;
                }
            }
        }
    }
}

/// Queue one leader frame for the browser. `None` means it was accepted.
///
/// Never awaits. Awaiting here is the shape to avoid: it would park this relay while the browser
/// catches up, which stops draining the unbounded queue inside [`LeaderClient`] and turns one slow
/// tab into unbounded memory growth. Dropping the connection instead is recoverable — the browser
/// reconnects and `session/load` replays the transcript — while silently dropping frames would
/// leave it rendering a transcript with holes in it that nothing would ever repair.
fn queue_for_browser(out_tx: &mpsc::Sender<Message>, payload: String) -> Option<RelayEnd> {
    match out_tx.try_send(Message::Text(payload.into())) {
        Ok(()) => None,
        Err(mpsc::error::TrySendError::Full(_)) => Some(RelayEnd::Backpressure),
        Err(mpsc::error::TrySendError::Closed(_)) => Some(RelayEnd::WriteFailed),
    }
}

/// Hand one browser frame to the leader. Returns `false` when the leader connection is gone.
///
/// Empty frames and the bare `ping` keepalive some browser clients send are skipped, matching
/// `grok agent serve`'s reader.
fn forward_to_leader(text: &str, to_leader: &mpsc::UnboundedSender<String>) -> bool {
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() || trimmed == "ping" {
        return true;
    }
    to_leader.send(trimmed.to_string()).is_ok()
}

/// Close frame for a finished relay, so the browser can tell "reconnect now" from "give up".
fn close_frame_for(
    end: RelayEnd,
    shutting_down: &watch::Receiver<Option<ShutdownReason>>,
) -> CloseFrame {
    match end {
        // 1001 going away: the leader is restarting (auto-update) or was stopped. Either way the
        // browser should reconnect; `connect_or_spawn` on the next attach adopts or spawns one.
        RelayEnd::LeaderGone => {
            let reason = match shutting_down.borrow().clone() {
                Some(ShutdownReason::AutoUpdate) => "leader restarting for update",
                Some(ShutdownReason::IdleTimeout) => "leader idle timeout",
                Some(ShutdownReason::Manual) => "leader shutting down",
                None => "leader connection lost",
            };
            CloseFrame {
                code: close_code::AWAY,
                reason: reason.into(),
            }
        }
        RelayEnd::Backpressure => CloseFrame {
            code: close_code::AGAIN,
            reason: "client too far behind".into(),
        },
        RelayEnd::ClientGone | RelayEnd::WriteFailed => CloseFrame {
            code: close_code::NORMAL,
            reason: "".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A browser is a thin view: it hosts no PTY and touches no files, so the agent must keep
    /// running terminals and file operations itself. See [`browser_capabilities`].
    #[test]
    fn browser_never_claims_terminal_or_filesystem() {
        let caps = browser_capabilities();
        assert!(!caps.terminal);
        assert!(!caps.fs_read);
        assert!(!caps.fs_write);
    }

    /// `Some(true)`, and specifically not `None`: absence is "did not say", which drops the agent back on its shared
    /// last-initialize-wins flag, so a TUI sharing this leader would decide whether a browser session is ever asked.
    #[test]
    fn browser_claims_the_folder_trust_card_per_client() {
        assert_eq!(browser_capabilities().interactive_trust, Some(true));
    }

    /// The registration `client_type` is load-bearing: `inject_session_request_context` early-returns
    /// when every capability is false and `client_type` is empty, and that injection is what keeps
    /// `clientTerminal`/`clientFsRead`/`clientFsWrite` per client instead of falling back to the
    /// agent's shared initialize state.
    #[test]
    fn client_type_is_non_empty_so_the_leader_injects_per_client_context() {
        assert!(!WEB_GATEWAY_CLIENT_TYPE.is_empty());
    }

    #[test]
    fn keepalive_and_control_frames_are_not_forwarded() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        assert!(forward_to_leader("ping", &tx));
        assert!(forward_to_leader("", &tx));
        assert!(forward_to_leader("{\"jsonrpc\":\"2.0\"}\n", &tx));
        assert_eq!(rx.try_recv().unwrap(), "{\"jsonrpc\":\"2.0\"}");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn forward_reports_a_dead_leader() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        assert!(!forward_to_leader("{}", &tx));
    }

    /// A browser that stops draining is disconnected, not queued without limit and not served with
    /// a transcript full of silent holes.
    #[test]
    fn a_browser_that_falls_behind_is_dropped_rather_than_buffered() {
        let (tx, _rx) = mpsc::channel(1);
        assert_eq!(queue_for_browser(&tx, "{}".into()), None);
        assert_eq!(
            queue_for_browser(&tx, "{}".into()),
            Some(RelayEnd::Backpressure)
        );
    }

    #[test]
    fn a_dead_writer_ends_the_relay() {
        let (tx, rx) = mpsc::channel(4);
        drop(rx);
        assert_eq!(
            queue_for_browser(&tx, "{}".into()),
            Some(RelayEnd::WriteFailed)
        );
    }

    #[test]
    fn a_planned_shutdown_is_named_in_the_close_frame() {
        let (_tx, rx) = watch::channel(Some(ShutdownReason::AutoUpdate));
        let frame = close_frame_for(RelayEnd::LeaderGone, &rx);
        assert_eq!(frame.code, close_code::AWAY);
        assert!(frame.reason.contains("update"));

        let (_tx, rx) = watch::channel(None);
        let frame = close_frame_for(RelayEnd::LeaderGone, &rx);
        assert_eq!(frame.code, close_code::AWAY);

        let frame = close_frame_for(RelayEnd::Backpressure, &rx);
        assert_eq!(frame.code, close_code::AGAIN);
    }
}
