//! Browser-side WebSocket transport to the emberviewer server.
//!
//! Outbound: the UI pushes [`ClientMsg`]s (sent as JSON text). Inbound: a task
//! reads the socket, turning JSON [`ServerMsg`]s and binary document frames into
//! [`WebEvent`]s the UI drains each frame. Single-threaded (browser), so `Rc` /
//! `RefCell` are fine and futures run via `spawn_local`.

use std::cell::RefCell;
use std::rc::Rc;

use ember_proto::glow::{self, Root};
use ember_web_proto::{decode_doc_frame, ClientMsg, ServerMsg, WireProvider, WireStatus};
use futures_util::{SinkExt, StreamExt};
use gloo_net::websocket::{futures::WebSocket, Message};

use crate::net::NetCommand;
use crate::wire::command_to_wire;

/// An event from the server to the browser UI.
pub enum WebEvent {
    AuthOk {
        open_lan: bool,
    },
    AuthRejected,
    Providers(Vec<WireProvider>),
    Status {
        id: u64,
        status: WireStatus,
    },
    Document {
        id: u64,
        root: Root,
    },
    Denied {
        reason: String,
    },
    /// The socket closed (or failed to open).
    Closed,
}

/// A live WebSocket connection to the server.
pub struct WsConnection {
    out_tx: futures_channel::mpsc::UnboundedSender<String>,
    inbox: Rc<RefCell<Vec<WebEvent>>>,
}

impl WsConnection {
    /// Open a connection to `url` (e.g. `ws://host:port/ws`). `ctx` is repainted
    /// whenever events arrive. Returns `None` if the socket can't be opened.
    pub fn connect(url: &str, ctx: egui::Context) -> Option<WsConnection> {
        let ws = WebSocket::open(url).ok()?;
        let (mut write, mut read) = ws.split();
        let (out_tx, mut out_rx) = futures_channel::mpsc::unbounded::<String>();
        let inbox: Rc<RefCell<Vec<WebEvent>>> = Rc::new(RefCell::new(Vec::new()));

        // Writer: forward queued JSON text frames to the socket.
        wasm_bindgen_futures::spawn_local(async move {
            while let Some(text) = out_rx.next().await {
                if write.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
        });

        // Reader: turn inbound frames into WebEvents.
        let rx_inbox = inbox.clone();
        wasm_bindgen_futures::spawn_local(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(t)) => {
                        if let Ok(sm) = ServerMsg::from_json(&t) {
                            push(&rx_inbox, server_msg_to_event(sm));
                        }
                    }
                    Ok(Message::Bytes(b)) => {
                        if let Some((id, ber)) = decode_doc_frame(&b) {
                            for root in glow::decode_roots(ber).into_iter().flatten() {
                                push(&rx_inbox, WebEvent::Document { id, root });
                            }
                        }
                    }
                    Err(_) => break,
                }
                ctx.request_repaint();
            }
            push(&rx_inbox, WebEvent::Closed);
            ctx.request_repaint();
        });

        Some(WsConnection { out_tx, inbox })
    }

    /// Send a control message.
    pub fn send(&self, msg: ClientMsg) {
        let _ = self.out_tx.unbounded_send(msg.to_json());
    }

    /// Send a command targeting a provider's connection.
    pub fn send_command(&self, provider_id: u64, cmd: &NetCommand) {
        self.send(ClientMsg::Command {
            id: provider_id,
            cmd: command_to_wire(cmd),
        });
    }

    /// Take all events buffered since the last drain.
    pub fn drain(&self) -> Vec<WebEvent> {
        std::mem::take(&mut self.inbox.borrow_mut())
    }
}

fn push(inbox: &Rc<RefCell<Vec<WebEvent>>>, ev: WebEvent) {
    inbox.borrow_mut().push(ev);
}

fn server_msg_to_event(sm: ServerMsg) -> WebEvent {
    match sm {
        ServerMsg::AuthOk { open_lan } => WebEvent::AuthOk { open_lan },
        ServerMsg::AuthRejected => WebEvent::AuthRejected,
        ServerMsg::Providers { providers } => WebEvent::Providers(providers),
        ServerMsg::Status { id, status } => WebEvent::Status { id, status },
        ServerMsg::Denied { reason } => WebEvent::Denied { reason },
    }
}
