//! Server mode: an HTTP + WebSocket server (axum) that lets browsers operate this
//! running instance. It serves the web bundle, exposes the address-book provider
//! list, and bridges each browser to the shared per-provider [`HubRegistry`] — so
//! every viewer (desktop + browsers) shares one connection per device.
//!
//! Wire vocabulary lives in [`ember_web_proto`]; documents cross as binary frames
//! (re-encoded Glow BER), control as JSON. See [`crate::wire`] for the mapping.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use ember_proto::glow;
use ember_web_proto::{encode_doc_frame, ClientMsg, ServerMsg, WireNode, WireProvider};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::oneshot;

use crate::hub::{HubLease, HubRegistry};
use crate::net::NetCommand;
use crate::wire::{command_from_wire, event_status};

/// What a browser may open: the flat provider list (for id→addr lookup) and the
/// folder tree (for the left pane). Kept in sync with the address book.
#[derive(Default)]
pub struct CatalogData {
    pub providers: Vec<WireProvider>,
    pub tree: Vec<WireNode>,
}

/// Shared, mutable catalog handed to the server.
pub type Catalog = Arc<Mutex<CatalogData>>;

/// Shared state handed to every request handler.
#[derive(Clone)]
struct ServerState {
    registry: HubRegistry,
    catalog: Catalog,
    token: String,
    open_lan: bool,
    read_only: bool,
    keepalive: bool,
}

/// A running server; dropping it triggers graceful shutdown.
pub struct ServerHandle {
    _shutdown: oneshot::Sender<()>,
    /// The address actually bound (for display).
    pub bound: SocketAddr,
}

/// Configuration for [`start`].
pub struct ServerConfig {
    pub port: u16,
    /// IP to bind to (`0.0.0.0` = all interfaces).
    pub bind: String,
    pub token: String,
    pub open_lan: bool,
    pub read_only: bool,
    pub keepalive: bool,
}

/// Bind and start the server on `rt`. Binding is synchronous so a port clash is
/// reported immediately.
pub fn start(
    rt: &tokio::runtime::Handle,
    registry: HubRegistry,
    catalog: Catalog,
    cfg: ServerConfig,
) -> std::io::Result<ServerHandle> {
    let ip: std::net::IpAddr = cfg
        .bind
        .parse()
        .unwrap_or(std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let std_listener = std::net::TcpListener::bind((ip, cfg.port))?;
    std_listener.set_nonblocking(true)?;
    let bound = std_listener.local_addr()?;

    let state = ServerState {
        registry,
        catalog,
        token: cfg.token,
        open_lan: cfg.open_lan,
        read_only: cfg.read_only,
        keepalive: cfg.keepalive,
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    rt.spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("server: failed to adopt listener: {e}");
                return;
            }
        };
        let app = router(state);
        let served = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(e) = served.await {
            tracing::error!("server: {e}");
        }
    });

    Ok(ServerHandle {
        _shutdown: shutdown_tx,
        bound,
    })
}

/// The embedded web UI bundle (built separately via wasm-bindgen).
#[derive(rust_embed::RustEmbed)]
#[folder = "web-dist/"]
struct WebAssets;

fn router(state: ServerState) -> Router {
    Router::new()
        .route("/api/providers", get(providers))
        .route("/ws", get(ws_upgrade))
        // The app shell (HTML/JS/WASM) is public; access control is on the data
        // (`/api/providers`) and the WebSocket. The token travels in the page URL.
        .fallback(static_asset)
        .with_state(state)
}

/// Serve an embedded asset (or `index.html` for `/`).
async fn static_asset(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match WebAssets::get(path) {
        Some(file) => (
            [(axum::http::header::CONTENT_TYPE, mime_for(path))],
            file.data,
        )
            .into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Content type for the handful of bundle asset kinds (`application/wasm` is
/// required for streaming instantiation).
fn mime_for(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".wasm") {
        "application/wasm"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

#[derive(serde::Deserialize)]
struct AuthQuery {
    token: Option<String>,
}

/// Access policy: open-LAN allows anyone; otherwise the token must match.
fn token_ok(open_lan: bool, expected: &str, got: Option<&str>) -> bool {
    open_lan || got == Some(expected)
}

/// True if `token` satisfies the access policy.
fn authorized(s: &ServerState, token: Option<&str>) -> bool {
    token_ok(s.open_lan, &s.token, token)
}

fn unauthorized() -> Response {
    (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response()
}

/// The provider list a browser may open.
async fn providers(State(s): State<ServerState>, Query(q): Query<AuthQuery>) -> Response {
    if !authorized(&s, q.token.as_deref()) {
        return unauthorized();
    }
    let list = s.catalog.lock().unwrap().providers.clone();
    Json(list).into_response()
}

async fn ws_upgrade(State(s): State<ServerState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, s))
}

/// Whether a command would change device state (gated when read-only).
fn is_mutating(cmd: &NetCommand) -> bool {
    matches!(
        cmd,
        NetCommand::SetValue(..) | NetCommand::MatrixConnect { .. } | NetCommand::Invoke { .. }
    )
}

async fn handle_ws(socket: WebSocket, s: ServerState) {
    let (mut sender, mut receiver) = socket.split();

    // Auth handshake: the first message must be a valid Auth.
    let authed = match receiver.next().await {
        Some(Ok(Message::Text(t))) => match ClientMsg::from_json(t.as_str()) {
            Ok(ClientMsg::Auth { token }) => authorized(&s, token.as_deref()),
            _ => false,
        },
        _ => false,
    };
    if !authed {
        let _ = sender
            .send(Message::Text(ServerMsg::AuthRejected.to_json().into()))
            .await;
        return;
    }
    let _ = sender
        .send(Message::Text(
            ServerMsg::AuthOk {
                open_lan: s.open_lan,
            }
            .to_json()
            .into(),
        ))
        .await;
    let (providers, nodes) = {
        let c = s.catalog.lock().unwrap();
        (c.providers.clone(), c.tree.clone())
    };
    let _ = sender
        .send(Message::Text(
            ServerMsg::Providers { providers }.to_json().into(),
        ))
        .await;
    let _ = sender
        .send(Message::Text(
            ServerMsg::AddressBook { nodes }.to_json().into(),
        ))
        .await;

    // One active provider per socket. Opening another replaces it.
    let mut current: Option<(u64, HubLease)> = None;

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(cm) = ClientMsg::from_json(t.as_str()) {
                            handle_client_msg(cm, &s, &mut current, &mut sender).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {} // ping/pong/binary from client: ignore
                }
            }
            event = next_event(&mut current) => {
                match event {
                    Some((id, ev)) => forward_event(id, &ev, &mut sender).await,
                    None => current = None, // that provider's connection ended
                }
            }
        }
    }
}

type WsSink = futures_util::stream::SplitSink<WebSocket, Message>;

/// Await the next event from the currently-open provider, or never (if none).
async fn next_event(
    current: &mut Option<(u64, HubLease)>,
) -> Option<(u64, std::sync::Arc<crate::net::NetEvent>)> {
    match current {
        Some((id, lease)) => lease.recv().await.map(|ev| (*id, ev)),
        None => std::future::pending().await,
    }
}

async fn handle_client_msg(
    cm: ClientMsg,
    s: &ServerState,
    current: &mut Option<(u64, HubLease)>,
    sender: &mut WsSink,
) {
    match cm {
        ClientMsg::Auth { .. } => {} // already authed
        ClientMsg::OpenProvider { id } => {
            let addr = s
                .catalog
                .lock()
                .unwrap()
                .providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| format!("{}:{}", p.host, p.port));
            if let Some(addr) = addr {
                let lease = s.registry.open(id, addr, s.keepalive);
                // Re-walk the root so this (possibly late-joining) client gets the
                // tree even though the shared connection was opened earlier.
                lease.send(NetCommand::GetDirectory(vec![]));
                *current = Some((id, lease));
            }
        }
        ClientMsg::CloseProvider { id } => {
            if current.as_ref().is_some_and(|(c, _)| *c == id) {
                *current = None;
            }
        }
        ClientMsg::Command { id, cmd } => {
            let Some(net_cmd) = command_from_wire(cmd) else {
                return;
            };
            if s.read_only && is_mutating(&net_cmd) {
                let _ = sender
                    .send(Message::Text(
                        ServerMsg::Denied {
                            reason: "server is read-only".into(),
                        }
                        .to_json()
                        .into(),
                    ))
                    .await;
                return;
            }
            if let Some((cur, lease)) = current.as_ref() {
                if *cur == id {
                    lease.send(net_cmd);
                }
            }
        }
    }
}

async fn forward_event(id: u64, ev: &crate::net::NetEvent, sender: &mut WsSink) {
    use crate::net::NetEvent;
    match ev {
        NetEvent::Document(root) => {
            if let Ok(ber) = glow::encode_root(root) {
                let _ = sender
                    .send(Message::Binary(encode_doc_frame(id, &ber).into()))
                    .await;
            }
        }
        other => {
            if let Some(status) = event_status(other) {
                let _ = sender
                    .send(Message::Text(
                        ServerMsg::Status { id, status }.to_json().into(),
                    ))
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::NetCommand;

    #[test]
    fn token_policy() {
        // Token mode: only the exact token passes.
        assert!(token_ok(false, "abc", Some("abc")));
        assert!(!token_ok(false, "abc", Some("xyz")));
        assert!(!token_ok(false, "abc", None));
        // Open-LAN: anything passes (including no token).
        assert!(token_ok(true, "abc", None));
        assert!(token_ok(true, "abc", Some("whatever")));
    }

    #[test]
    fn mutating_commands_are_gated() {
        assert!(is_mutating(&NetCommand::SetValue(
            vec![0],
            ember_proto::glow::Value::Boolean(true)
        )));
        assert!(is_mutating(&NetCommand::MatrixConnect {
            path: vec![1],
            target: 0,
            sources: vec![],
            operation: 0,
        }));
        assert!(is_mutating(&NetCommand::Invoke {
            path: vec![1],
            invocation_id: 0,
            args: vec![],
        }));
        assert!(!is_mutating(&NetCommand::GetDirectory(vec![0])));
        assert!(!is_mutating(&NetCommand::Subscribe(vec![0])));
    }

    #[test]
    fn binds_and_reports_port_then_clashes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reg = HubRegistry::new(rt.handle().clone(), egui::Context::default());
        let cat: Catalog = Arc::new(Mutex::new(CatalogData::default()));
        let cfg = ServerConfig {
            port: 0,
            bind: "127.0.0.1".into(),
            token: "t".into(),
            open_lan: false,
            read_only: false,
            keepalive: false,
        };
        let h = start(rt.handle(), reg.clone(), cat.clone(), cfg).expect("bind ephemeral");
        let port = h.bound.port();
        assert_ne!(port, 0);
        // Re-binding the same concrete port should fail (address in use).
        let clash = start(
            rt.handle(),
            reg,
            cat,
            ServerConfig {
                port,
                bind: "127.0.0.1".into(),
                token: "t".into(),
                open_lan: false,
                read_only: false,
                keepalive: false,
            },
        );
        assert!(clash.is_err(), "second bind on {port} should clash");
    }

    async fn http_get(port: u16, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[test]
    fn http_provider_list_is_token_gated() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reg = HubRegistry::new(rt.handle().clone(), egui::Context::default());
        let cat: Catalog = Arc::new(Mutex::new(CatalogData {
            providers: vec![WireProvider {
                id: 5,
                name: "Ruby".into(),
                host: "10.0.0.2".into(),
                port: 9000,
            }],
            tree: Vec::new(),
        }));
        let h = start(
            rt.handle(),
            reg,
            cat,
            ServerConfig {
                port: 0,
                bind: "127.0.0.1".into(),
                token: "secret".into(),
                open_lan: false,
                read_only: false,
                keepalive: false,
            },
        )
        .unwrap();
        let port = h.bound.port();
        rt.block_on(async {
            let no_token = http_get(port, "/api/providers").await;
            assert!(
                no_token.starts_with("HTTP/1.1 401"),
                "expected 401, got: {}",
                &no_token[..no_token.len().min(32)]
            );
            let ok = http_get(port, "/api/providers?token=secret").await;
            assert!(ok.starts_with("HTTP/1.1 200"), "expected 200");
            assert!(ok.contains("Ruby"), "provider name missing from body");
        });
    }

    #[test]
    fn serves_the_web_shell_without_a_token() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reg = HubRegistry::new(rt.handle().clone(), egui::Context::default());
        let cat: Catalog = Arc::new(Mutex::new(CatalogData::default()));
        let h = start(
            rt.handle(),
            reg,
            cat,
            ServerConfig {
                port: 0,
                bind: "127.0.0.1".into(),
                token: "secret".into(),
                open_lan: false,
                read_only: false,
                keepalive: false,
            },
        )
        .unwrap();
        let port = h.bound.port();
        rt.block_on(async {
            // The app shell is public (no token); content-type is HTML.
            let resp = http_get(port, "/").await;
            assert!(resp.starts_with("HTTP/1.1 200"), "expected 200 for /");
            assert!(resp.contains("text/html"), "index should be html");
        });
    }
}
