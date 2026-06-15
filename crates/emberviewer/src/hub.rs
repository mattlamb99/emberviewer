//! Fan-out hub: one Ember+ connection, many subscribers.
//!
//! A [`Hub`] owns the per-provider connection task and hands out [`Subscriber`]s
//! (the desktop UI today; web clients later). Inbound documents/status are
//! broadcast to every subscriber; outbound commands from any subscriber are
//! merged into the single connection. Device-level Subscribe/Unsubscribe are
//! reference-counted so the provider - often an embedded device with a tight
//! connection/subscription budget - sees a single consumer no matter how many
//! viewers are attached. Dropping the `Hub` shuts the connection down.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use ember_net::{ConnError, Connection, Inbound, ProviderWriter, Traffic, TrafficSnapshot};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::net::{NetCommand, NetEvent};

/// Identifies one subscriber, for subscription ref-counting.
type SubscriberId = u64;

/// A path's set of interested subscribers.
type SubMap = HashMap<Vec<u32>, HashSet<SubscriberId>>;

/// Broadcast buffer; generous so a brief UI stall during the initial tree walk
/// doesn't drop documents.
const EVENT_CAPACITY: usize = 8192;
const MAX_BACKOFF_SECS: u64 = 30;

/// A message from a subscriber to the connection task.
enum HubMsg {
    Cmd {
        id: SubscriberId,
        cmd: NetCommand,
    },
    Drop(SubscriberId),
    /// Reconnect to a new address (the provider's host:port was edited).
    Retarget(String),
}

/// Owns one provider connection and fans it out to [`Subscriber`]s.
pub struct Hub {
    msg_tx: mpsc::UnboundedSender<HubMsg>,
    evt_tx: broadcast::Sender<Arc<NetEvent>>,
    next_id: AtomicU64,
    // Shared byte/frame counters for this provider's socket (read by the UI).
    traffic: Arc<Traffic>,
    // Dropped when the Hub is dropped, which signals the connection task to stop.
    _shutdown: oneshot::Sender<()>,
}

impl Hub {
    /// Spawn a connection task on `rt` connecting to `addr`. `ctx` wakes the UI
    /// when events arrive.
    pub fn spawn(
        rt: &tokio::runtime::Handle,
        addr: String,
        ctx: egui::Context,
        keepalive: bool,
    ) -> Hub {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (evt_tx, _) = broadcast::channel(EVENT_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let traffic = Arc::new(Traffic::default());
        rt.spawn(run_connection(
            addr,
            msg_rx,
            evt_tx.clone(),
            ctx,
            keepalive,
            traffic.clone(),
            shutdown_rx,
        ));
        Hub {
            msg_tx,
            evt_tx,
            next_id: AtomicU64::new(0),
            traffic,
            _shutdown: shutdown_tx,
        }
    }

    /// Current cumulative byte/frame totals for this provider's socket.
    pub fn traffic_snapshot(&self) -> TrafficSnapshot {
        self.traffic.snapshot()
    }

    /// Move this connection to a new address: the task drops its current socket
    /// and reconnects there, keeping every subscriber attached.
    pub fn retarget(&self, addr: String) {
        let _ = self.msg_tx.send(HubMsg::Retarget(addr));
    }

    /// Attach a new subscriber (the desktop UI, or a web client).
    pub fn subscribe(&self) -> Subscriber {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Subscriber {
            id,
            msg_tx: self.msg_tx.clone(),
            evt_rx: self.evt_tx.subscribe(),
        }
    }
}

/// One consumer of a [`Hub`]: sends commands in, drains events out.
pub struct Subscriber {
    id: SubscriberId,
    msg_tx: mpsc::UnboundedSender<HubMsg>,
    evt_rx: broadcast::Receiver<Arc<NetEvent>>,
}

impl Subscriber {
    /// Send a command (ignored if the connection has ended).
    pub fn send(&self, cmd: NetCommand) {
        let _ = self.msg_tx.send(HubMsg::Cmd { id: self.id, cmd });
    }

    /// Drain all pending events for this subscriber (sync; for the egui frame
    /// loop).
    pub fn drain(&mut self) -> Vec<NetEvent> {
        use broadcast::error::TryRecvError;
        let mut out = Vec::new();
        loop {
            match self.evt_rx.try_recv() {
                Ok(ev) => out.push((*ev).clone()),
                // Lagged: this receiver fell behind and skipped some events; keep
                // draining the rest rather than stalling.
                Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
            }
        }
        out
    }

    /// Await the next event (async; for the WebSocket server). `None` once the
    /// connection has ended.
    pub async fn recv(&mut self) -> Option<Arc<NetEvent>> {
        use broadcast::error::RecvError;
        loop {
            match self.evt_rx.recv().await {
                Ok(ev) => return Some(ev),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

/// A shared registry of live provider connections, keyed by provider id. A
/// [`Hub`] is created on first open and shut down once the last [`HubLease`]
/// (desktop session or web client) drops - so the device sees one consumer no
/// matter how many viewers attach. Cheap to clone (everything is shared).
#[derive(Clone)]
pub struct HubRegistry {
    inner: Arc<Mutex<HashMap<u64, Weak<Hub>>>>,
    rt: tokio::runtime::Handle,
    ctx: egui::Context,
}

impl HubRegistry {
    pub fn new(rt: tokio::runtime::Handle, ctx: egui::Context) -> Self {
        HubRegistry {
            inner: Arc::new(Mutex::new(HashMap::new())),
            rt,
            ctx,
        }
    }

    /// Attach a viewer to provider `id`, reusing the live connection if one
    /// exists or spawning it (connecting to `addr`) otherwise.
    pub fn open(&self, id: u64, addr: String, keepalive: bool) -> HubLease {
        let mut map = self.inner.lock().unwrap();
        let hub = match map.get(&id).and_then(Weak::upgrade) {
            Some(h) => h,
            None => {
                let h = Arc::new(Hub::spawn(&self.rt, addr, self.ctx.clone(), keepalive));
                map.insert(id, Arc::downgrade(&h));
                h
            }
        };
        let sub = hub.subscribe();
        HubLease { _hub: hub, sub }
    }

    /// Retarget the live hub for `id` (if any) to `addr`, so all its viewers move
    /// to the new endpoint together. A no-op when nothing is connected to `id`
    /// (the next [`open`](Self::open) will simply use the new address).
    pub fn retarget(&self, id: u64, addr: String) {
        let map = self.inner.lock().unwrap();
        if let Some(hub) = map.get(&id).and_then(Weak::upgrade) {
            hub.retarget(addr);
        }
    }
}

/// A viewer's lease on a shared [`Hub`]: keeps the connection alive while held,
/// and carries this viewer's [`Subscriber`].
pub struct HubLease {
    _hub: Arc<Hub>,
    sub: Subscriber,
}

impl HubLease {
    pub fn send(&self, cmd: NetCommand) {
        self.sub.send(cmd);
    }
    pub fn drain(&mut self) -> Vec<NetEvent> {
        self.sub.drain()
    }
    pub async fn recv(&mut self) -> Option<Arc<NetEvent>> {
        self.sub.recv().await
    }
    /// Cumulative socket byte/frame totals for this provider (shared across viewers).
    pub fn traffic(&self) -> TrafficSnapshot {
        self._hub.traffic_snapshot()
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        // Release this subscriber's subscriptions (ref-count drops on the task).
        let _ = self.msg_tx.send(HubMsg::Drop(self.id));
    }
}

// ---------------------------------------------------------------------------
// Connection task
// ---------------------------------------------------------------------------

/// Why a session loop ended.
enum SessionEnd {
    UserDisconnect,
    Shutdown,
    Dropped(String),
    /// Reconnect immediately to a new address (the provider was retargeted).
    Retarget(String),
}

#[allow(clippy::too_many_arguments)]
async fn run_connection(
    mut addr: String,
    mut msg_rx: mpsc::UnboundedReceiver<HubMsg>,
    evt_tx: broadcast::Sender<Arc<NetEvent>>,
    ctx: egui::Context,
    keepalive: bool,
    traffic: Arc<Traffic>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let emit = |e: NetEvent| {
        let _ = evt_tx.send(Arc::new(e));
        ctx.request_repaint();
    };

    // Subscriptions wanted across all subscribers; persists across reconnects so
    // the device can be re-subscribed when the link comes back.
    let mut subs: SubMap = HashMap::new();
    let mut backoff = 1u64;

    loop {
        let conn = tokio::select! {
            biased;
            _ = &mut shutdown_rx => { emit(NetEvent::Disconnected(None)); return; }
            c = Connection::connect(&addr) => c,
        };
        match conn {
            Ok(conn) => {
                backoff = 1;
                emit(NetEvent::Connected);
                match run_session(
                    conn,
                    &mut msg_rx,
                    &emit,
                    keepalive,
                    &traffic,
                    &mut subs,
                    &mut shutdown_rx,
                )
                .await
                {
                    SessionEnd::UserDisconnect | SessionEnd::Shutdown => {
                        emit(NetEvent::Disconnected(None));
                        return;
                    }
                    SessionEnd::Dropped(reason) => emit(NetEvent::Reconnecting {
                        retry_in_secs: backoff,
                        reason,
                    }),
                    SessionEnd::Retarget(new_addr) => {
                        // New endpoint: reconnect at once (no backoff) and tell
                        // viewers to drop the old device's tree.
                        addr = new_addr;
                        backoff = 1;
                        emit(NetEvent::Retargeted);
                        continue;
                    }
                }
            }
            Err(e) => emit(NetEvent::Reconnecting {
                retry_in_secs: backoff,
                reason: e.to_string(),
            }),
        }

        // Wait out the backoff - cancellable by shutdown, a Disconnect, or a
        // retarget (which reconnects immediately to the new address).
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => { emit(NetEvent::Disconnected(None)); return; }
            _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
            msg = msg_rx.recv() => match msg {
                None => { emit(NetEvent::Disconnected(None)); return; }
                Some(HubMsg::Cmd { cmd: NetCommand::Disconnect, .. }) => {
                    emit(NetEvent::Disconnected(None));
                    return;
                }
                Some(HubMsg::Retarget(new_addr)) => {
                    addr = new_addr;
                    backoff = 1;
                    emit(NetEvent::Retargeted);
                    continue;
                }
                // No live writer during backoff; just keep the ref-counts current.
                Some(HubMsg::Cmd { id, cmd }) => record_sub_intent(&mut subs, id, &cmd),
                Some(HubMsg::Drop(id)) => { release_subscriber(&mut subs, id); }
            },
        }
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }
}

/// Drive one live connection until it drops, the user disconnects, or shutdown.
#[allow(clippy::too_many_arguments)]
async fn run_session(
    conn: Connection,
    msg_rx: &mut mpsc::UnboundedReceiver<HubMsg>,
    emit: &impl Fn(NetEvent),
    keepalive: bool,
    traffic: &Arc<Traffic>,
    subs: &mut SubMap,
    shutdown_rx: &mut oneshot::Receiver<()>,
) -> SessionEnd {
    let (mut reader, mut writer) = conn.into_split_with(traffic.clone());

    // Kick off discovery at the root, then restore any active subscriptions -
    // after a reconnect the device has forgotten them.
    if let Err(e) = writer.get_directory(&[]).await {
        emit(NetEvent::Error(e.to_string()));
    }
    for path in subs.keys() {
        if let Err(e) = writer.subscribe(path).await {
            return SessionEnd::Dropped(e.to_string());
        }
    }

    let mut keepalive_timer = tokio::time::interval(Duration::from_secs(2));
    keepalive_timer.tick().await; // skip the immediate first tick

    loop {
        tokio::select! {
            biased;
            _ = &mut *shutdown_rx => return SessionEnd::Shutdown,
            _ = keepalive_timer.tick(), if keepalive => {
                if let Err(e) = writer.keepalive_request().await {
                    return SessionEnd::Dropped(e.to_string());
                }
            }
            msg = msg_rx.recv() => {
                let result = match msg {
                    None => return SessionEnd::UserDisconnect,
                    Some(HubMsg::Cmd { cmd: NetCommand::Disconnect, .. }) => {
                        return SessionEnd::UserDisconnect
                    }
                    Some(HubMsg::Retarget(addr)) => return SessionEnd::Retarget(addr),
                    Some(HubMsg::Drop(id)) => unsubscribe_released(&mut writer, subs, id).await,
                    Some(HubMsg::Cmd { id, cmd }) => {
                        apply_command(&mut writer, subs, id, cmd).await
                    }
                };
                if let Err(e) = result {
                    // A write failure means the link is gone - drop and reconnect.
                    return SessionEnd::Dropped(e.to_string());
                }
            }
            inbound = reader.recv() => match inbound {
                Ok(Some(Inbound::Documents { roots, raw })) => {
                    emit(NetEvent::Document {
                        roots: Arc::new(roots),
                        raw: Arc::new(raw),
                    });
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

/// Apply one command to the live writer, ref-counting Subscribe/Unsubscribe so
/// the device only sees the 0↔1 transitions.
async fn apply_command(
    writer: &mut ProviderWriter,
    subs: &mut SubMap,
    id: SubscriberId,
    cmd: NetCommand,
) -> Result<(), ConnError> {
    match cmd {
        NetCommand::GetDirectory(path) => writer.get_directory(&path).await,
        NetCommand::GetMatrixDirectory(path) => writer.get_matrix_directory(&path).await,
        NetCommand::SetValue(path, value) => writer.set_value(&path, value).await,
        NetCommand::Subscribe(path) => {
            if sub_add(subs, &path, id) {
                writer.subscribe(&path).await
            } else {
                Ok(())
            }
        }
        NetCommand::Unsubscribe(path) => {
            if sub_remove(subs, &path, id) {
                writer.unsubscribe(&path).await
            } else {
                Ok(())
            }
        }
        NetCommand::MatrixConnect {
            path,
            target,
            sources,
            operation,
        } => {
            writer
                .matrix_connect(&path, target, &sources, operation)
                .await
        }
        NetCommand::Invoke {
            path,
            invocation_id,
            args,
        } => writer.invoke(&path, invocation_id, args).await,
        NetCommand::Disconnect => Ok(()), // handled by the caller
    }
}

/// A subscriber went away: drop its refs and unsubscribe any now-orphaned paths.
async fn unsubscribe_released(
    writer: &mut ProviderWriter,
    subs: &mut SubMap,
    id: SubscriberId,
) -> Result<(), ConnError> {
    for path in release_subscriber(subs, id) {
        writer.unsubscribe(&path).await?;
    }
    Ok(())
}

/// Add `id` as interested in `path`; returns `true` on the 0→1 transition (the
/// device should be subscribed).
fn sub_add(subs: &mut SubMap, path: &[u32], id: SubscriberId) -> bool {
    let set = subs.entry(path.to_vec()).or_default();
    let was_empty = set.is_empty();
    set.insert(id);
    was_empty
}

/// Remove `id`'s interest in `path`; returns `true` on the 1→0 transition (the
/// device should be unsubscribed).
fn sub_remove(subs: &mut SubMap, path: &[u32], id: SubscriberId) -> bool {
    if let Some(set) = subs.get_mut(path) {
        set.remove(&id);
        if set.is_empty() {
            subs.remove(path);
            return true;
        }
    }
    false
}

/// Update ref-counts for a Subscribe/Unsubscribe without touching the device
/// (used while disconnected; the device is re-subscribed on reconnect).
fn record_sub_intent(subs: &mut SubMap, id: SubscriberId, cmd: &NetCommand) {
    match cmd {
        NetCommand::Subscribe(path) => {
            sub_add(subs, path, id);
        }
        NetCommand::Unsubscribe(path) => {
            sub_remove(subs, path, id);
        }
        _ => {}
    }
}

/// Remove `id` from every path; return the paths that became unreferenced.
fn release_subscriber(subs: &mut SubMap, id: SubscriberId) -> Vec<Vec<u32>> {
    let mut emptied = Vec::new();
    subs.retain(|path, set| {
        set.remove(&id);
        if set.is_empty() {
            emptied.push(path.clone());
            false
        } else {
            true
        }
    });
    emptied
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &[u32] = &[1, 1];
    const B: &[u32] = &[1, 2];

    #[test]
    fn device_subscribe_only_on_first_and_unsubscribe_only_on_last() {
        let mut subs = SubMap::new();
        // Two subscribers want the same path: only the first triggers the device.
        assert!(
            sub_add(&mut subs, A, 1),
            "first subscriber → device subscribe"
        );
        assert!(
            !sub_add(&mut subs, A, 2),
            "second subscriber → no device call"
        );
        // First leaves: still referenced, no device unsubscribe.
        assert!(
            !sub_remove(&mut subs, A, 1),
            "still referenced → no device call"
        );
        // Last leaves: device unsubscribe, path forgotten.
        assert!(
            sub_remove(&mut subs, A, 2),
            "last subscriber → device unsubscribe"
        );
        assert!(subs.is_empty());
    }

    #[test]
    fn idempotent_and_unknown_paths_are_safe() {
        let mut subs = SubMap::new();
        assert!(sub_add(&mut subs, A, 1));
        // Re-subscribing the same id is a no-op transition.
        assert!(!sub_add(&mut subs, A, 1));
        // Unsubscribing an id/path we never had does nothing.
        assert!(!sub_remove(&mut subs, B, 9));
        assert!(!sub_remove(&mut subs, A, 9));
    }

    #[test]
    fn retarget_unknown_id_is_noop() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reg = HubRegistry::new(rt.handle().clone(), egui::Context::default());
        // Nothing is connected to id 42; retargeting it must not panic.
        reg.retarget(42, "127.0.0.1:9000".into());
    }

    #[test]
    fn release_returns_only_newly_orphaned_paths() {
        let mut subs = SubMap::new();
        // id 1 holds A and B; id 2 also holds A.
        sub_add(&mut subs, A, 1);
        sub_add(&mut subs, A, 2);
        sub_add(&mut subs, B, 1);
        // Dropping id 1: A is still held by id 2, B becomes orphaned.
        let orphaned = release_subscriber(&mut subs, 1);
        assert_eq!(orphaned, vec![B.to_vec()]);
        assert!(subs.contains_key(A));
        assert!(!subs.contains_key(B));
        // Dropping id 2: A orphaned.
        let orphaned = release_subscriber(&mut subs, 2);
        assert_eq!(orphaned, vec![A.to_vec()]);
        assert!(subs.is_empty());
    }
}
