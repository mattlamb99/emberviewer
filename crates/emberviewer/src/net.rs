//! Bridge between the egui UI (main thread) and async Ember+ connections.
//!
//! Each connection runs as a task on a shared tokio runtime. The UI sends
//! [`NetCommand`]s down an unbounded channel and drains [`NetEvent`]s each frame.
//! Whenever an event is produced we request an egui repaint so the UI wakes up.

use std::sync::mpsc;

use ember_net::{Connection, Inbound};
use ember_proto::glow::{Root, Value};
use tokio::sync::mpsc as tokio_mpsc;

/// A command from the UI to a connection task.
#[derive(Debug)]
pub enum NetCommand {
    /// Request the directory (children) of the node at this path (empty = root).
    GetDirectory(Vec<u32>),
    /// Set the parameter at this path to a new value.
    SetValue(Vec<u32>, Value),
    /// Subscribe to value changes of the parameter at this path.
    Subscribe(Vec<u32>),
    /// Unsubscribe from value changes of the parameter at this path.
    Unsubscribe(Vec<u32>),
    /// Close the connection.
    Disconnect,
}

/// An event from a connection task to the UI.
#[derive(Debug)]
pub enum NetEvent {
    /// Freshly connected (initial or after a reconnect).
    Connected,
    /// A decoded Glow document to merge into the tree.
    Document(Root),
    /// The connection dropped; will retry in `retry_in_secs`.
    Reconnecting { retry_in_secs: u64, reason: String },
    /// The connection ended for good (user disconnect or fatal).
    Disconnected(Option<String>),
    /// A non-fatal error.
    Error(String),
}

/// UI-side handle to a running connection.
pub struct ConnectionHandle {
    cmd_tx: tokio_mpsc::UnboundedSender<NetCommand>,
    evt_rx: mpsc::Receiver<NetEvent>,
}

impl ConnectionHandle {
    /// Spawn a connection task on `rt` connecting to `addr`. `ctx` is used to
    /// wake the UI when events arrive.
    pub fn spawn(
        rt: &tokio::runtime::Handle,
        addr: String,
        ctx: egui::Context,
        keepalive: bool,
    ) -> ConnectionHandle {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        rt.spawn(run_connection(addr, cmd_rx, evt_tx, ctx, keepalive));
        ConnectionHandle { cmd_tx, evt_rx }
    }

    /// Send a command (ignored if the task has ended).
    pub fn send(&self, cmd: NetCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Drain all pending events.
    pub fn drain(&self) -> Vec<NetEvent> {
        self.evt_rx.try_iter().collect()
    }
}

const MAX_BACKOFF_SECS: u64 = 30;

async fn run_connection(
    addr: String,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<NetCommand>,
    evt_tx: mpsc::Sender<NetEvent>,
    ctx: egui::Context,
    keepalive: bool,
) {
    let emit = |e: NetEvent| -> bool {
        let ok = evt_tx.send(e).is_ok();
        ctx.request_repaint();
        ok
    };

    let mut backoff = 1u64;
    loop {
        match Connection::connect(&addr).await {
            Ok(conn) => {
                backoff = 1; // reset on a successful connect
                if !emit(NetEvent::Connected) {
                    return;
                }
                match run_session(conn, &mut cmd_rx, &emit, keepalive).await {
                    SessionEnd::UserDisconnect => {
                        emit(NetEvent::Disconnected(None));
                        return;
                    }
                    SessionEnd::Dropped(reason) => {
                        if !emit(NetEvent::Reconnecting {
                            retry_in_secs: backoff,
                            reason,
                        }) {
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                if !emit(NetEvent::Reconnecting {
                    retry_in_secs: backoff,
                    reason: e.to_string(),
                }) {
                    return;
                }
            }
        }

        // Wait out the backoff, but let a Disconnect command cancel the retry.
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff)) => {}
            cmd = cmd_rx.recv() => {
                if matches!(cmd, Some(NetCommand::Disconnect) | None) {
                    emit(NetEvent::Disconnected(None));
                    return;
                }
            }
        }
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }
}

/// Why a session loop ended.
enum SessionEnd {
    UserDisconnect,
    Dropped(String),
}

/// Drive one live connection until it drops or the user disconnects.
async fn run_session(
    conn: Connection,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<NetCommand>,
    emit: &impl Fn(NetEvent) -> bool,
    keepalive: bool,
) -> SessionEnd {
    let (mut reader, mut writer) = conn.into_split();

    // Kick off discovery at the root.
    if let Err(e) = writer.get_directory(&[]).await {
        emit(NetEvent::Error(e.to_string()));
    }

    let mut keepalive_timer = tokio::time::interval(std::time::Duration::from_secs(2));
    // Skip the immediate first tick.
    keepalive_timer.tick().await;

    loop {
        tokio::select! {
            _ = keepalive_timer.tick(), if keepalive => {
                if let Err(e) = writer.keepalive_request().await {
                    return SessionEnd::Dropped(e.to_string());
                }
            }
            cmd = cmd_rx.recv() => {
                let result = match cmd {
                    Some(NetCommand::GetDirectory(path)) => writer.get_directory(&path).await,
                    Some(NetCommand::SetValue(path, value)) => writer.set_value(&path, value).await,
                    Some(NetCommand::Subscribe(path)) => writer.subscribe(&path).await,
                    Some(NetCommand::Unsubscribe(path)) => writer.unsubscribe(&path).await,
                    Some(NetCommand::Disconnect) | None => return SessionEnd::UserDisconnect,
                };
                if let Err(e) = result {
                    // A write failure means the link is gone — drop and reconnect.
                    return SessionEnd::Dropped(e.to_string());
                }
            }
            inbound = reader.recv() => match inbound {
                Ok(Some(Inbound::Documents(roots))) => {
                    for root in roots {
                        emit(NetEvent::Document(root));
                    }
                }
                Ok(Some(Inbound::KeepAliveRequest)) => {
                    let _ = writer.keepalive_response().await;
                }
                Ok(None) => return SessionEnd::Dropped("connection closed by provider".into()),
                Err(e) => return SessionEnd::Dropped(e.to_string()),
            },
        }
    }
}
