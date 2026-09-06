//! End-to-end coverage for the browser WebSocket gateway (`grok agent gateway`).
//!
//! The gateway's whole claim is that it adds authentication without adding a second session model:
//! each browser becomes an ordinary leader client, so the leader's tested N-client semantics apply
//! unchanged. These tests hold it to that by putting a WebSocket client and a plain
//! [`LeaderClient`] on the same leader and the same session.
//!
//! The leader here runs with no agent behind it: `spawn_leader_server` hands the test `acp_rx` and
//! `response_tx`, so the test plays the agent. That keeps the cases about routing and lifecycle
//! rather than about model inference, which is what the PTY suite in `xai-grok-pager` covers.
//!
//! Unix-only for the same reason as `test_leader_stdio_integration`: the leader socket is a Unix
//! domain socket here.

#![cfg(unix)]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use xai_grok_shell::agent::web_gateway::{attach_at_socket, serve_web_gateway};
use xai_grok_shell::leader::{
    ClientCapabilities, ClientMode, LeaderClient, ServerHandle, spawn_leader_server,
};

const SECRET: &str = "gateway-integration-secret";
const STEP: Duration = Duration::from_secs(5);

/// Open the harness's keeper registration, retrying until the leader's socket is bound.
async fn connect_keeper(sock_path: &std::path::Path) -> LeaderClient {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match LeaderClient::connect(
            sock_path.to_path_buf(),
            "harness-keeper",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        {
            Ok(client) => return client,
            Err(e) => assert!(
                tokio::time::Instant::now() < deadline,
                "timeout waiting for the leader socket: {e}"
            ),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Registrations the harness itself holds; see [`Harness::keeper`].
const KEEPER: usize = 1;

/// A leader with no agent, plus a gateway in front of it on an ephemeral loopback port.
struct Harness {
    _temp: TempDir,
    sock_path: std::path::PathBuf,
    leader: ServerHandle,
    gateway_addr: std::net::SocketAddr,
    /// One long-lived registration held for the whole test.
    ///
    /// `spawn_leader_server` builds a leader with `no_exit_on_disconnect: false`, so it shuts down
    /// and unlinks its socket the moment its client count returns to zero after having been
    /// non-zero. Every case here takes the count to zero at some point — that is the lifecycle
    /// being tested — so without a keeper the leader would vanish mid-test. Waiting for the socket
    /// by connecting and dropping a probe stream is the same hazard in miniature: the probe is a
    /// client, and its close is what would end the leader.
    _keeper: LeaderClient,
}

impl Harness {
    async fn start() -> Self {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("leader.sock");
        let leader = spawn_leader_server(sock_path.clone()).await.unwrap();
        // Doubles as the readiness wait: the first successful registration is proof of a bound
        // socket, and it is a registration the leader keeps rather than one it counts and loses.
        let keeper = connect_keeper(&sock_path).await;

        // Port 0: the OS picks the port, so concurrent test binaries never collide.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = listener.local_addr().unwrap();
        let attach = attach_at_socket(sock_path.clone());
        tokio::spawn(async move {
            let _ = serve_web_gateway(listener, SECRET.to_string(), attach).await;
        });

        Self {
            _temp: temp,
            sock_path,
            leader,
            gateway_addr,
            _keeper: keeper,
        }
    }

    fn ws_url(&self, key: &str) -> String {
        format!(
            "ws://{}/ws?server-key={}",
            self.gateway_addr,
            urlencoding::encode(key)
        )
    }

    async fn connect_browser(
        &self,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let (ws, _) = tokio_tungstenite::connect_async(self.ws_url(SECRET))
            .await
            .expect("gateway accepts an authenticated browser");
        ws
    }

    async fn connect_leader_client(&self, client_type: &str) -> LeaderClient {
        LeaderClient::connect(
            self.sock_path.clone(),
            client_type,
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap()
    }

    /// Next ACP payload the leader forwarded towards the agent.
    async fn next_agent_request(&mut self) -> serde_json::Value {
        let raw = tokio::time::timeout(STEP, self.leader.acp_rx.recv())
            .await
            .expect("timeout waiting for the leader to forward a request")
            .expect("leader closed the agent channel");
        serde_json::from_str(&raw).expect("leader forwarded valid JSON")
    }

    /// Wait for the leader's registered-client count to settle on `want`.
    async fn wait_for_client_count(&self, want: usize) {
        let deadline = tokio::time::Instant::now() + STEP;
        loop {
            let now = self
                .leader
                .client_count
                .load(std::sync::atomic::Ordering::Relaxed);
            if now == want {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "client count stuck at {now}, wanted {want}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Next text frame the browser receives, parsed as JSON.
async fn next_browser_json<S>(ws: &mut S) -> serde_json::Value
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let frame = tokio::time::timeout(STEP, ws.next())
            .await
            .expect("timeout waiting for a frame at the browser")
            .expect("gateway closed the socket")
            .expect("websocket error");
        match frame {
            Message::Text(text) => {
                return serde_json::from_str(&text).expect("gateway relayed valid JSON");
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame at the browser: {other:?}"),
        }
    }
}

/// The leader has no authentication of its own, so the gateway is the only thing standing between a
/// browser and the machine's agent. A wrong or missing key must be refused at the HTTP upgrade,
/// before any leader registration exists.
#[tokio::test]
async fn gateway_refuses_a_browser_without_the_secret() {
    let harness = Harness::start().await;

    for url in [
        harness.ws_url("not-the-secret"),
        format!("ws://{}/ws", harness.gateway_addr),
    ] {
        let outcome = tokio_tungstenite::connect_async(&url).await;
        assert!(
            outcome.is_err(),
            "gateway accepted an unauthenticated connection to {url}"
        );
    }

    // The refused attempts opened no registration: only the harness keeper is still counted.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        harness
            .leader
            .client_count
            .load(std::sync::atomic::Ordering::Relaxed),
        KEEPER,
    );

    harness.leader.cancel.cancel();
}

/// The property the whole design rests on: a browser behind the gateway and a native leader client
/// are two ordinary subscribers of one session, so an agent notification reaches both.
///
/// If the gateway multiplexed browsers onto a single shared registration (the shape `agent serve`
/// has), or terminated ACP itself, this could not hold.
#[tokio::test]
async fn browser_and_native_client_share_one_session() {
    let mut harness = Harness::start().await;

    let mut browser = harness.connect_browser().await;
    let mut native = harness.connect_leader_client("grok-tui").await;
    harness.wait_for_client_count(KEEPER + 2).await;

    // The leader subscribes a client to a session the first time it sees a session-scoped message
    // from it. Both clients claim the same session id.
    let shared = "sess-shared-browser-and-tui";
    browser
        .send(Message::text(format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"session/setModel","params":{{"sessionId":"{shared}","modelId":"grok-4"}}}}"#
        )))
        .await
        .unwrap();
    let forwarded = harness.next_agent_request().await;
    assert_eq!(forwarded["method"], "session/setModel");
    assert_eq!(forwarded["params"]["sessionId"], shared);
    // The browser's request reached the leader namespaced, i.e. as a first-class client's request.
    let browser_request_id = forwarded["id"]
        .as_str()
        .expect("the leader namespaces client request ids")
        .to_string();

    native
        .send(format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"session/setModel","params":{{"sessionId":"{shared}","modelId":"grok-4"}}}}"#
        ))
        .unwrap();
    let forwarded = harness.next_agent_request().await;
    assert_eq!(forwarded["params"]["sessionId"], shared);

    // A session-scoped notification from the agent fans out to every subscriber.
    harness
        .leader
        .response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","method":"x.ai/session_notification","params":{{"sessionId":"{shared}","update":{{"sessionUpdate":"model_changed","model_id":"grok-4"}}}}}}"#
        ))
        .unwrap();

    let at_browser = next_browser_json(&mut browser).await;
    assert_eq!(at_browser["method"], "x.ai/session_notification");
    assert_eq!(at_browser["params"]["sessionId"], shared);

    let at_native = tokio::time::timeout(STEP, native.recv())
        .await
        .expect("timeout waiting for the fan-out at the native client")
        .expect("native client channel closed");
    let at_native: serde_json::Value = serde_json::from_str(&at_native).unwrap();
    assert_eq!(at_native["method"], "x.ai/session_notification");
    assert_eq!(at_native["params"]["sessionId"], shared);

    // The targeted response still goes only to the client that asked.
    harness
        .leader
        .response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","id":"{browser_request_id}","result":{{"ok":true}}}}"#
        ))
        .unwrap();
    let at_browser = next_browser_json(&mut browser).await;
    assert_eq!(at_browser["id"], 1, "the leader un-namespaces the reply");
    assert_eq!(at_browser["result"]["ok"], true);

    harness.leader.cancel.cancel();
}

/// A browser is a thin view onto a local machine: it hosts no PTY and touches no files.
///
/// The leader stamps the registering client's capabilities into every session request's `_meta`,
/// and that is what the agent reads per client (`resolve_client_io_caps`). This pins what the
/// gateway puts on the wire, because the fallback when `_meta` is absent is shared agent state.
#[tokio::test]
async fn browser_sessions_declare_no_terminal_and_no_filesystem() {
    let mut harness = Harness::start().await;
    let mut browser = harness.connect_browser().await;
    harness.wait_for_client_count(KEEPER + 1).await;

    browser
        .send(Message::text(
            r#"{"jsonrpc":"2.0","id":7,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}"#,
        ))
        .await
        .unwrap();

    let forwarded = harness.next_agent_request().await;
    assert_eq!(forwarded["method"], "session/new");
    let meta = &forwarded["params"]["_meta"];
    assert_eq!(meta["clientTerminal"], false);
    assert_eq!(meta["clientFsRead"], false);
    assert_eq!(meta["clientFsWrite"], false);
    // Non-empty client identity is what makes the leader inject at all, and what maps the browser
    // to `ClientType::GrokWeb` agent-side.
    assert_eq!(meta["clientIdentifier"], "grok-web");

    harness.leader.cancel.cancel();
}

/// A browser that disappears mid-session must not leave a registration behind.
///
/// Dropping the channels alone would not do it: the leader client's write task keeps sending
/// keepalive pings, so the leader would still count the client and still list it as a subscriber of
/// a session nobody is watching.
#[tokio::test]
async fn a_closed_browser_releases_its_leader_registration() {
    let harness = Harness::start().await;

    let mut browser = harness.connect_browser().await;
    harness.wait_for_client_count(KEEPER + 1).await;

    browser.close(None).await.unwrap();
    drop(browser);

    harness.wait_for_client_count(KEEPER).await;
    harness.leader.cancel.cancel();
}

/// Killing the browser's socket without a close handshake must be handled the same way.
#[tokio::test]
async fn an_aborted_browser_releases_its_leader_registration() {
    let harness = Harness::start().await;

    let browser = harness.connect_browser().await;
    harness.wait_for_client_count(KEEPER + 1).await;

    // No close frame: just drop the TCP connection, as a crashed tab does.
    drop(browser);

    harness.wait_for_client_count(KEEPER).await;
    harness.leader.cancel.cancel();
}

/// When the leader shuts down, the browser must be told rather than left on a socket that silently
/// stops producing. 1001 (going away) marks it reconnectable; the next attach adopts or spawns a
/// leader through the ordinary discovery path.
#[tokio::test]
async fn leader_shutdown_closes_the_browser_socket() {
    let harness = Harness::start().await;
    let mut browser = harness.connect_browser().await;
    harness.wait_for_client_count(KEEPER + 1).await;

    harness.leader.cancel.cancel();

    let close = loop {
        let frame = tokio::time::timeout(STEP, browser.next())
            .await
            .expect("timeout waiting for the gateway to close the browser socket");
        match frame {
            Some(Ok(Message::Close(frame))) => break frame,
            // tungstenite reports the peer's close as an error after it has echoed the handshake.
            Some(Err(_)) | None => break None,
            Some(Ok(_)) => continue,
        }
    };

    if let Some(close) = close {
        assert_eq!(
            u16::from(close.code),
            1001,
            "a leader shutdown must read as going-away, not a protocol failure"
        );
        assert!(
            close.reason.contains("leader"),
            "close reason should name the leader: {}",
            close.reason
        );
    }
}

/// A browser that stops reading must not hold up the leader or the browsers next to it.
///
/// The leader already refuses to block on a slow client — it pushes into a per-client unbounded
/// queue with `try_send` — and the gateway keeps that promise on its side of the socket by never
/// awaiting a browser write from the relay loop. Here the stalled tab never calls `next()`, so it
/// never even reads the frames the OS has buffered for it.
#[tokio::test]
async fn a_stalled_browser_does_not_hold_up_the_others() {
    let mut harness = Harness::start().await;

    let mut stalled = harness.connect_browser().await;
    let mut reader = harness.connect_browser().await;
    harness.wait_for_client_count(KEEPER + 2).await;

    // Both subscribe to the same session, then `stalled` never reads again.
    let shared = "sess-stalled-neighbour";
    for (i, browser) in [&mut stalled, &mut reader].into_iter().enumerate() {
        browser
            .send(Message::text(format!(
                r#"{{"jsonrpc":"2.0","id":{i},"method":"session/setModel","params":{{"sessionId":"{shared}","modelId":"grok-4"}}}}"#
            )))
            .await
            .unwrap();
        let forwarded = harness.next_agent_request().await;
        assert_eq!(forwarded["params"]["sessionId"], shared);
    }

    // Enough traffic that a gateway which awaited the stalled socket would be parked by now.
    const BURST: usize = 200;
    for i in 0..BURST {
        harness
            .leader
            .response_tx
            .send(format!(
                r#"{{"jsonrpc":"2.0","method":"x.ai/session_notification","params":{{"sessionId":"{shared}","seq":{i}}}}}"#
            ))
            .unwrap();
    }

    for i in 0..BURST {
        let msg = next_browser_json(&mut reader).await;
        assert_eq!(
            msg["params"]["seq"], i,
            "the reading browser lost frames behind a stalled neighbour"
        );
    }

    // The leader is still serving: a fresh request from the healthy browser still round-trips.
    reader
        .send(Message::text(format!(
            r#"{{"jsonrpc":"2.0","id":99,"method":"session/setModel","params":{{"sessionId":"{shared}","modelId":"grok-4"}}}}"#
        )))
        .await
        .unwrap();
    let forwarded = harness.next_agent_request().await;
    assert_eq!(forwarded["method"], "session/setModel");

    harness.leader.cancel.cancel();
}

/// Several browsers at once are just several leader clients: none of them displaces another, which
/// is exactly what `agent serve`'s single relay slot cannot do.
#[tokio::test]
async fn many_browsers_hold_independent_registrations() {
    let mut harness = Harness::start().await;

    let mut browsers = Vec::new();
    for _ in 0..3 {
        browsers.push(harness.connect_browser().await);
    }
    harness.wait_for_client_count(KEEPER + 3).await;

    let shared = "sess-many-browsers";
    for (i, browser) in browsers.iter_mut().enumerate() {
        browser
            .send(Message::text(format!(
                r#"{{"jsonrpc":"2.0","id":{i},"method":"session/setModel","params":{{"sessionId":"{shared}","modelId":"grok-4"}}}}"#
            )))
            .await
            .unwrap();
        let forwarded = harness.next_agent_request().await;
        assert_eq!(forwarded["params"]["sessionId"], shared);
    }

    harness
        .leader
        .response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","method":"x.ai/session_notification","params":{{"sessionId":"{shared}","update":{{"sessionUpdate":"model_changed","model_id":"grok-4"}}}}}}"#
        ))
        .unwrap();

    for (i, browser) in browsers.iter_mut().enumerate() {
        let msg = next_browser_json(browser).await;
        assert_eq!(
            msg["method"], "x.ai/session_notification",
            "browser {i} missed the fan-out"
        );
    }

    harness.leader.cancel.cancel();
}
