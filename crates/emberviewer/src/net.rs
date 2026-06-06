//! Bridge between the egui UI (main thread) and async Ember+ connections.
//!
//! Each connection runs as a task on a shared tokio runtime, owned by a
//! [`crate::hub::Hub`] that fans one connection out to many subscribers. The UI
//! holds a [`ConnectionHandle`] (one subscriber): it sends [`NetCommand`]s and
//! drains [`NetEvent`]s each frame. Whenever an event is produced the connection
//! task requests an egui repaint so the UI wakes up.

use ember_proto::glow::{Root, Value};

use crate::hub::{Hub, Subscriber};

/// A command from a consumer to a connection.
#[derive(Debug, Clone)]
pub enum NetCommand {
    /// Request the directory (children) of the node at this path (empty = root).
    GetDirectory(Vec<u32>),
    /// Request a matrix's directory, addressed as a matrix so the provider
    /// returns its targets/sources/connections (Lawo needs this addressing).
    GetMatrixDirectory(Vec<u32>),
    /// Set the parameter at this path to a new value.
    SetValue(Vec<u32>, Value),
    /// Subscribe to value changes of the parameter at this path.
    Subscribe(Vec<u32>),
    /// Unsubscribe from value changes of the parameter at this path.
    Unsubscribe(Vec<u32>),
    /// Change a matrix crosspoint: matrix path, target, sources, operation.
    MatrixConnect {
        path: Vec<u32>,
        target: u32,
        sources: Vec<u32>,
        operation: i32,
    },
    /// Invoke a function: path, invocation id, arguments.
    Invoke {
        path: Vec<u32>,
        invocation_id: i32,
        args: Vec<Value>,
    },
    /// Close the connection.
    Disconnect,
}

/// An event from a connection to its consumers.
#[derive(Debug, Clone)]
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

/// UI-side handle to a running connection: a [`Hub`] (owning the connection) and
/// the desktop's own [`Subscriber`] on it. Dropping the handle drops the Hub,
/// which shuts the connection down.
pub struct ConnectionHandle {
    _hub: Hub,
    sub: Subscriber,
}

impl ConnectionHandle {
    /// Spawn a connection on `rt` connecting to `addr`. `ctx` wakes the UI when
    /// events arrive.
    pub fn spawn(
        rt: &tokio::runtime::Handle,
        addr: String,
        ctx: egui::Context,
        keepalive: bool,
    ) -> ConnectionHandle {
        let hub = Hub::spawn(rt, addr, ctx, keepalive);
        let sub = hub.subscribe();
        ConnectionHandle { _hub: hub, sub }
    }

    /// Send a command (ignored if the connection has ended).
    pub fn send(&self, cmd: NetCommand) {
        self.sub.send(cmd);
    }

    /// Drain all pending events.
    pub fn drain(&mut self) -> Vec<NetEvent> {
        self.sub.drain()
    }
}
