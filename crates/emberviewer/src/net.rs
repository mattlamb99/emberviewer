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
    Connected,
    /// A decoded Glow document to merge into the tree.
    Document(Root),
    /// The connection ended; carries an optional reason.
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
    ) -> ConnectionHandle {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        rt.spawn(run_connection(addr, cmd_rx, evt_tx, ctx));
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

async fn run_connection(
    addr: String,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<NetCommand>,
    evt_tx: mpsc::Sender<NetEvent>,
    ctx: egui::Context,
) {
    // Helper to emit an event and wake the UI.
    macro_rules! emit {
        ($e:expr) => {{
            if evt_tx.send($e).is_err() {
                return;
            }
            ctx.request_repaint();
        }};
    }

    let conn = match Connection::connect(&addr).await {
        Ok(c) => c,
        Err(e) => {
            emit!(NetEvent::Disconnected(Some(e.to_string())));
            return;
        }
    };
    emit!(NetEvent::Connected);

    let (mut reader, mut writer) = conn.into_split();

    // Kick off discovery at the root.
    if let Err(e) = writer.get_directory(&[]).await {
        emit!(NetEvent::Error(e.to_string()));
    }

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(NetCommand::GetDirectory(path)) => {
                    if let Err(e) = writer.get_directory(&path).await {
                        emit!(NetEvent::Error(e.to_string()));
                    }
                }
                Some(NetCommand::SetValue(path, value)) => {
                    if let Err(e) = writer.set_value(&path, value).await {
                        emit!(NetEvent::Error(e.to_string()));
                    }
                }
                Some(NetCommand::Subscribe(path)) => {
                    if let Err(e) = writer.subscribe(&path).await {
                        emit!(NetEvent::Error(e.to_string()));
                    }
                }
                Some(NetCommand::Unsubscribe(path)) => {
                    if let Err(e) = writer.unsubscribe(&path).await {
                        emit!(NetEvent::Error(e.to_string()));
                    }
                }
                Some(NetCommand::Disconnect) | None => {
                    emit!(NetEvent::Disconnected(None));
                    return;
                }
            },
            inbound = reader.recv() => match inbound {
                Ok(Some(Inbound::Root(root))) => emit!(NetEvent::Document(root)),
                Ok(Some(Inbound::KeepAliveRequest)) => {
                    let _ = writer.keepalive_response().await;
                }
                Ok(None) => {
                    emit!(NetEvent::Disconnected(None));
                    return;
                }
                Err(e) => {
                    emit!(NetEvent::Disconnected(Some(e.to_string())));
                    return;
                }
            },
        }
    }
}
