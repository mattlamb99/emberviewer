//! Async TCP transport for Ember+: S101 framing over a [`tokio`] socket.

use std::time::Duration;

use ember_proto::glow::{self, Root, Value};
use ember_proto::s101::{self, FrameDecoder, Incoming};
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

/// A live connection to an Ember+ provider.
///
/// Provides a simple request/response style API suitable for headless walking;
/// the GUI layer drives this from a dedicated task and bridges to the UI over
/// channels. Inbound keep-alive requests are answered automatically.
pub struct Connection {
    stream: TcpStream,
    decoder: FrameDecoder,
    read_buf: Vec<u8>,
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
            // Drain any messages already buffered in the decoder first.
            // (We re-push an empty slice to surface nothing; real work happens
            // after a read below.)
            let n = self.stream.read(&mut self.read_buf).await?;
            if n == 0 {
                return Ok(None);
            }
            let incoming = self.decoder.push(&self.read_buf[..n]);
            for item in incoming {
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
        let (read, write) = self.stream.into_split();
        (
            ProviderReader {
                read,
                decoder: self.decoder,
                read_buf: self.read_buf,
            },
            ProviderWriter { write },
        )
    }
}

/// Something received from the provider on the read half.
#[derive(Debug)]
pub enum Inbound {
    /// One or more decoded Glow documents from a single message.
    Documents(Vec<Root>),
    /// The provider asked us to keep the connection alive; reply via the writer.
    KeepAliveRequest,
}

/// A short hex preview of a payload, for diagnostics.
fn hex_preview(bytes: &[u8]) -> String {
    const MAX: usize = 512;
    let shown: String = bytes
        .iter()
        .take(MAX)
        .map(|b| format!("{b:02x}"))
        .collect();
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
}

impl ProviderReader {
    /// Await the next inbound item. Returns `Ok(None)` when the peer closes.
    pub async fn recv(&mut self) -> Result<Option<Inbound>, ConnError> {
        loop {
            let n = self.read.read(&mut self.read_buf).await?;
            if n == 0 {
                return Ok(None);
            }
            for item in self.decoder.push(&self.read_buf[..n]) {
                match item {
                    Ok(Incoming::EmberPayload(payload)) => {
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
                            return Ok(Some(Inbound::Documents(roots)));
                        }
                        // Nothing decoded — keep reading.
                    }
                    Ok(Incoming::KeepAliveRequest) => {
                        return Ok(Some(Inbound::KeepAliveRequest));
                    }
                    Ok(Incoming::KeepAliveResponse) | Ok(Incoming::ProviderState(_)) => {}
                    Err(e) => tracing::warn!("dropping malformed S101 frame: {e}"),
                }
            }
        }
    }
}

/// Write half of a split [`Connection`].
pub struct ProviderWriter {
    write: OwnedWriteHalf,
}

impl ProviderWriter {
    /// Send a Glow `Root` document.
    pub async fn send(&mut self, root: &Root) -> Result<(), ConnError> {
        let payload = glow::encode_root(root).map_err(|e| ConnError::Encode(e.to_string()))?;
        let frames = s101::encode_ember(&payload);
        self.write.write_all(&frames).await?;
        self.write.flush().await?;
        Ok(())
    }

    /// Request the directory of the node at `path` (empty = root).
    pub async fn get_directory(&mut self, path: &[u32]) -> Result<(), ConnError> {
        self.send(&Root::get_directory_at(path)).await
    }

    /// Set the parameter at `path` to `value`.
    pub async fn set_value(&mut self, path: &[u32], value: Value) -> Result<(), ConnError> {
        self.send(&Root::set_value_at(path, value)).await
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
        self.write
            .write_all(&s101::encode_keepalive_response())
            .await?;
        self.write.flush().await?;
        Ok(())
    }

    /// Send a keep-alive request.
    pub async fn keepalive_request(&mut self) -> Result<(), ConnError> {
        self.write
            .write_all(&s101::encode_keepalive_request())
            .await?;
        self.write.flush().await?;
        Ok(())
    }
}
