//! Bridge between the egui UI (main thread) and async Ember+ connections.
//!
//! Each connection runs as a task on a shared tokio runtime, owned by a
//! [`crate::hub::Hub`] that fans one connection out to many subscribers. The UI
//! holds a [`ConnectionHandle`] (one subscriber): it sends [`NetCommand`]s and
//! drains [`NetEvent`]s each frame. Whenever an event is produced the connection
//! task requests an egui repaint so the UI wakes up.

use std::sync::Arc;

use ember_proto::glow::{Root, Value};

#[cfg(not(target_arch = "wasm32"))]
use crate::hub::{HubLease, HubRegistry};

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
// Constructed only on native (hub/server); the wasm client uses its own event
// type, so the variants look unused there.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Debug, Clone)]
pub enum NetEvent {
    /// Freshly connected (initial or after a reconnect).
    Connected,
    /// The decoded Glow documents from one provider message, to merge into the
    /// tree, plus the original BER bytes they came from. The desktop UI merges
    /// `roots`; the server forwards `raw` verbatim so a browser decodes exactly
    /// what `ember-net` decoded (re-encoding the parsed tree could be lossy for
    /// vendor extensions the tolerant decoder keeps but the encoder can't restore).
    Document {
        roots: Arc<Vec<Root>>,
        raw: Arc<Vec<u8>>,
    },
    /// The connection dropped; will retry in `retry_in_secs`.
    Reconnecting { retry_in_secs: u64, reason: String },
    /// The connection's target address changed; it is reconnecting to the new
    /// endpoint. Viewers should drop any cached tree and await fresh documents.
    Retargeted,
    /// The connection ended for good (user disconnect or fatal).
    Disconnected(Option<String>),
    /// A non-fatal error.
    Error(String),
}

/// UI-side handle to a running connection: the desktop's [`HubLease`] on the
/// shared per-provider Hub. Dropping it releases the desktop's view (and shuts
/// the connection down if no other viewer - e.g. a browser - holds it).
#[cfg(not(target_arch = "wasm32"))]
pub struct ConnectionHandle {
    lease: HubLease,
}

#[cfg(not(target_arch = "wasm32"))]
impl ConnectionHandle {
    /// Attach the desktop as a viewer of provider `id` (connecting to `addr` if
    /// it isn't already open).
    pub fn open(
        registry: &HubRegistry,
        id: u64,
        addr: String,
        keepalive: bool,
    ) -> ConnectionHandle {
        ConnectionHandle {
            lease: registry.open(id, addr, keepalive),
        }
    }

    /// Send a command (ignored if the connection has ended).
    pub fn send(&self, cmd: NetCommand) {
        self.lease.send(cmd);
    }

    /// Drain all pending events.
    pub fn drain(&mut self) -> Vec<NetEvent> {
        self.lease.drain()
    }

    /// Cumulative socket byte/frame totals toward this provider.
    pub fn traffic(&self) -> ember_net::TrafficSnapshot {
        self.lease.traffic()
    }
}
