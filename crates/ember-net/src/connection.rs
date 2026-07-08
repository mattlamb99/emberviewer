//! Async TCP transport for Ember+: S101 framing over a [`tokio`] socket.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ember_proto::glow::{self, Root, Value};
use ember_proto::s101::{self, FrameDecoder, Incoming, S101Error};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpStream, ToSocketAddrs};

/// Default Ember+ TCP port.
pub const DEFAULT_PORT: u16 = 9000;

#[derive(Debug, thiserror::Error)]
pub enum ConnError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("BER decode error: {0}")]
    Decode(String),
    #[error("BER encode error: {0}")]
    Encode(String),
    #[error("connection closed by peer")]
    Closed,
}

/// Cumulative byte and S101-frame counters for one connection, shared between the
/// read/write halves and the UI. `rx` counts what the device sends us, `tx` what we
/// send the device. Bytes are raw socket bytes (including S101 framing and
/// keep-alives); frames are whole S101 messages.
#[derive(Debug, Default)]
pub struct Traffic {
    rx_bytes: AtomicU64,
    tx_bytes: AtomicU64,
    rx_frames: AtomicU64,
    tx_frames: AtomicU64,
}

impl Traffic {
    fn record_tx(&self, bytes: usize) {
        self.tx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.tx_frames.fetch_add(1, Ordering::Relaxed);
    }

    /// A point-in-time copy of the cumulative totals.
    pub fn snapshot(&self) -> TrafficSnapshot {
        TrafficSnapshot {
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            rx_frames: self.rx_frames.load(Ordering::Relaxed),
            tx_frames: self.tx_frames.load(Ordering::Relaxed),
        }
    }
}

/// A snapshot of [`Traffic`]'s cumulative counters (bytes/frames each way).
#[derive(Debug, Clone, Copy, Default)]
pub struct TrafficSnapshot {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_frames: u64,
    pub tx_frames: u64,
}

/// A live connection to an Ember+ provider.
///
/// Provides a simple request/response style API suitable for headless walking;
/// the GUI layer drives this from a dedicated task and bridges to the UI over
/// channels. Inbound keep-alive requests are answered automatically.
pub struct Connection {
    stream: TcpStream,
    decoder: FrameDecoder,
    read_buf: Vec<u8>,
    /// Frames decoded from a previous read but not yet delivered. One TCP read
    /// can carry several S101 frames while each call returns at most one item,
    /// so the surplus queues here and is drained before the next socket read.
    pending: VecDeque<Result<Incoming, S101Error>>,
}

impl Connection {
    /// Connect to a provider.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, ConnError> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true).ok();
        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
            read_buf: vec![0u8; 16 * 1024],
            pending: VecDeque::new(),
        })
    }

    /// Send a Glow `Root` document to the provider.
    pub async fn send(&mut self, root: &Root) -> Result<(), ConnError> {
        let payload = glow::encode_root(root).map_err(|e| ConnError::Encode(e.to_string()))?;
        let frames = s101::encode_ember(&payload);
        self.stream.write_all(&frames).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Convenience: request the directory of the node at `path` (empty = root).
    pub async fn get_directory(&mut self, path: &[u32]) -> Result<(), ConnError> {
        self.send(&Root::get_directory_at(path)).await
    }

    /// Send a keep-alive request.
    pub async fn send_keepalive(&mut self) -> Result<(), ConnError> {
        self.stream
            .write_all(&s101::encode_keepalive_request())
            .await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Read the next Glow `Root` document, transparently answering keep-alive
    /// requests. Returns `Ok(None)` if the peer closes the connection.
    pub async fn next_root(&mut self) -> Result<Option<Root>, ConnError> {
        loop {
            // Drain frames left over from a previous read before touching the
            // socket - returning early for one payload must not lose the rest.
            while let Some(item) = self.pending.pop_front() {
                match item {
                    Ok(Incoming::EmberPayload(payload)) => {
                        let root = glow::decode_root(&payload)
                            .map_err(|e| ConnError::Decode(e.to_string()))?;
                        return Ok(Some(root));
                    }
                    Ok(Incoming::KeepAliveRequest) => {
                        self.stream
                            .write_all(&s101::encode_keepalive_response())
                            .await?;
                        self.stream.flush().await?;
                    }
                    Ok(Incoming::KeepAliveResponse) | Ok(Incoming::ProviderState(_)) => {}
                    Err(e) => {
                        tracing::warn!("dropping malformed S101 frame: {e}");
                    }
                }
            }
            let n = self.stream.read(&mut self.read_buf).await?;
            if n == 0 {
                return Ok(None);
            }
            self.pending.extend(self.decoder.push(&self.read_buf[..n]));
        }
    }

    /// Read the next `Root`, giving up after `timeout`.
    pub async fn next_root_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Root>, ConnError> {
        match tokio::time::timeout(timeout, self.next_root()).await {
            Ok(res) => res,
            Err(_) => Ok(None),
        }
    }

    /// Split into independent read and write halves so a caller can `select!`
    /// over reading and writing concurrently (as the GUI's connection actor does).
    pub fn into_split(self) -> (ProviderReader, ProviderWriter) {
        self.into_split_with(Arc::new(Traffic::default()))
    }

    /// Like [`into_split`](Self::into_split) but sharing `traffic`, so the caller
    /// (the GUI) can read byte/frame totals - and persist them across reconnects.
    pub fn into_split_with(self, traffic: Arc<Traffic>) -> (ProviderReader, ProviderWriter) {
        let (read, write) = self.stream.into_split();
        (
            ProviderReader {
                read,
                decoder: self.decoder,
                read_buf: self.read_buf,
                pending: self.pending,
                traffic: traffic.clone(),
            },
            ProviderWriter { write, traffic },
        )
    }
}

/// Something received from the provider on the read half.
#[derive(Debug)]
pub enum Inbound {
    /// One or more decoded Glow documents from a single message, alongside the
    /// original BER payload they were decoded from. The raw bytes are kept so a
    /// consumer (the server) can forward them verbatim to a remote viewer, which
    /// then decodes byte-identically - re-encoding `roots` would risk a lossy or
    /// asymmetric result for vendor extensions the tolerant decoder preserves but
    /// the encoder can't reproduce exactly.
    Documents { roots: Vec<Root>, raw: Vec<u8> },
    /// The provider asked us to keep the connection alive; reply via the writer.
    KeepAliveRequest,
    /// The provider answered our keep-alive request - proof the link is alive,
    /// for a caller that wants to detect a vanished peer faster than TCP's own
    /// (multi-minute) retransmit timeout.
    KeepAliveResponse,
}

/// Whether full hex dumping of every Ember+ frame (sent and received) is on.
/// Logged via `tracing` at info level; the GUI's "Enable debug log" option flips
/// this at runtime, and `EMBER_DUMP=1` seeds it at startup (developer workflow).
static FRAME_DUMP: AtomicBool = AtomicBool::new(false);

/// Turn frame dumping on or off at runtime.
pub fn set_frame_dump(on: bool) {
    FRAME_DUMP.store(on, Ordering::Relaxed);
}

/// Whether frame dumping is currently enabled.
pub fn frame_dump_enabled() -> bool {
    FRAME_DUMP.load(Ordering::Relaxed)
}

/// Seed frame dumping from the `EMBER_DUMP` env var. Call once at startup before
/// the GUI may override it; keeps the old developer env-var workflow working.
pub fn init_frame_dump_from_env() {
    let on = std::env::var("EMBER_DUMP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    set_frame_dump(on);
}

/// Full hex of a payload.
fn full_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A short hex preview of a payload, for diagnostics.
fn hex_preview(bytes: &[u8]) -> String {
    const MAX: usize = 512;
    let shown: String = bytes.iter().take(MAX).map(|b| format!("{b:02x}")).collect();
    if bytes.len() > MAX {
        format!("{shown}… ({} bytes total)", bytes.len())
    } else {
        shown
    }
}

/// Read half of a split [`Connection`].
pub struct ProviderReader {
    read: OwnedReadHalf,
    decoder: FrameDecoder,
    read_buf: Vec<u8>,
    /// Frames decoded but not yet delivered - see [`Connection::pending`].
    pending: VecDeque<Result<Incoming, S101Error>>,
    traffic: Arc<Traffic>,
}

impl ProviderReader {
    /// Await the next inbound item. Returns `Ok(None)` when the peer closes.
    pub async fn recv(&mut self) -> Result<Option<Inbound>, ConnError> {
        loop {
            // Drain frames left over from a previous read before touching the
            // socket - returning early for one payload must not lose the rest.
            while let Some(item) = self.pending.pop_front() {
                self.traffic.rx_frames.fetch_add(1, Ordering::Relaxed);
                match item {
                    Ok(Incoming::EmberPayload(payload)) => {
                        if frame_dump_enabled() {
                            tracing::info!(
                                "RX payload {} bytes: {}",
                                payload.len(),
                                full_hex(&payload)
                            );
                        }
                        let mut roots = Vec::new();
                        for result in glow::decode_roots(&payload) {
                            match result {
                                Ok(root) => roots.push(root),
                                Err(e) => tracing::warn!(
                                    "BER decode error: {e}; payload={}",
                                    hex_preview(&payload)
                                ),
                            }
                        }
                        if !roots.is_empty() {
                            return Ok(Some(Inbound::Documents {
                                roots,
                                raw: payload,
                            }));
                        }
                        // Nothing decoded - keep reading.
                    }
                    Ok(Incoming::KeepAliveRequest) => {
                        return Ok(Some(Inbound::KeepAliveRequest));
                    }
                    Ok(Incoming::KeepAliveResponse) => {
                        return Ok(Some(Inbound::KeepAliveResponse));
                    }
                    Ok(Incoming::ProviderState(_)) => {}
                    Err(e) => tracing::warn!("dropping malformed S101 frame: {e}"),
                }
            }
            let n = self.read.read(&mut self.read_buf).await?;
            if n == 0 {
                return Ok(None);
            }
            self.traffic.rx_bytes.fetch_add(n as u64, Ordering::Relaxed);
            self.pending.extend(self.decoder.push(&self.read_buf[..n]));
        }
    }
}

/// Write half of a split [`Connection`].
pub struct ProviderWriter {
    write: OwnedWriteHalf,
    traffic: Arc<Traffic>,
}

impl ProviderWriter {
    /// Send a Glow `Root` document.
    pub async fn send(&mut self, root: &Root) -> Result<(), ConnError> {
        let payload = glow::encode_root(root).map_err(|e| ConnError::Encode(e.to_string()))?;
        if frame_dump_enabled() {
            tracing::info!("TX payload {} bytes: {}", payload.len(), full_hex(&payload));
        }
        let frames = s101::encode_ember(&payload);
        self.write.write_all(&frames).await?;
        self.write.flush().await?;
        self.traffic.record_tx(frames.len());
        Ok(())
    }

    /// Request the directory of the node at `path` (empty = root).
    pub async fn get_directory(&mut self, path: &[u32]) -> Result<(), ConnError> {
        self.send(&Root::get_directory_at(path)).await
    }

    /// Request a matrix's directory, addressed as a matrix (returns connections).
    pub async fn get_matrix_directory(&mut self, path: &[u32]) -> Result<(), ConnError> {
        self.send(&Root::get_matrix_directory_at(path)).await
    }

    /// Set the parameter at `path` to `value`.
    pub async fn set_value(&mut self, path: &[u32], value: Value) -> Result<(), ConnError> {
        self.send(&Root::set_value_at(path, value)).await
    }

    /// Re-read the parameter at `path` (returns its current value).
    pub async fn get_parameter(&mut self, path: &[u32]) -> Result<(), ConnError> {
        self.send(&Root::get_parameter_at(path)).await
    }

    /// Subscribe to value changes of the parameter at `path`.
    pub async fn subscribe(&mut self, path: &[u32]) -> Result<(), ConnError> {
        self.send(&Root::subscribe_at(path)).await
    }

    /// Unsubscribe from value changes of the parameter at `path`.
    pub async fn unsubscribe(&mut self, path: &[u32]) -> Result<(), ConnError> {
        self.send(&Root::unsubscribe_at(path)).await
    }

    /// Change a matrix crosspoint.
    pub async fn matrix_connect(
        &mut self,
        path: &[u32],
        target: u32,
        sources: &[u32],
        operation: i32,
    ) -> Result<(), ConnError> {
        self.send(&Root::matrix_connect(path, target, sources, operation))
            .await
    }

    /// Invoke a function with arguments.
    pub async fn invoke(
        &mut self,
        path: &[u32],
        invocation_id: i32,
        args: Vec<Value>,
    ) -> Result<(), ConnError> {
        self.send(&Root::invoke(path, invocation_id, args)).await
    }

    /// Reply to a provider keep-alive request.
    pub async fn keepalive_response(&mut self) -> Result<(), ConnError> {
        let frame = s101::encode_keepalive_response();
        self.write.write_all(&frame).await?;
        self.write.flush().await?;
        self.traffic.record_tx(frame.len());
        Ok(())
    }

    /// Send a keep-alive request.
    pub async fn keepalive_request(&mut self) -> Result<(), ConnError> {
        let frame = s101::encode_keepalive_request();
        self.write.write_all(&frame).await?;
        self.write.flush().await?;
        self.traffic.record_tx(frame.len());
        Ok(())
    }
}
