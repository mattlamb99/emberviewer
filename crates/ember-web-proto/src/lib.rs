//! WebSocket wire vocabulary shared by the emberviewer **server** (the native
//! app in server mode) and its **browser client** (the wasm build).
//!
//! Two frame kinds cross the socket:
//! - **Text frames** carry JSON [`ClientMsg`]/[`ServerMsg`] - auth, the provider
//!   list, open/close, status, and commands.
//! - **Binary frames** carry Glow documents verbatim: a [`DocFrame`] is a little
//!   `[u64 provider_id][BER bytes…]`, so the client decodes them with the same
//!   `ember_proto::glow::decode_roots` it would use against a real device - no
//!   serde shadow of the large `Root`/`Value`/`Matrix` types is needed.
//!
//! This crate deliberately has **no `ember-proto` dependency**: it is just the
//! vocabulary. The emberviewer crate owns the conversions between its internal
//! `NetCommand`/`NetEvent`/`Value` and the [`WireCommand`]/[`WireValue`] here, so
//! both the native server and the wasm client share that one mapping.

use serde::{Deserialize, Serialize};

/// A parameter value on the wire. Mirrors `ember_proto::glow::Value`; octets are
/// base64 so JSON stays compact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireValue {
    Int(i64),
    Real(f64),
    Str(String),
    Bool(bool),
    /// Octet string, base64-encoded.
    Oct(String),
}

/// A command from a client to a provider. Mirrors `emberviewer`'s `NetCommand`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum WireCommand {
    GetDirectory {
        path: Vec<u32>,
    },
    GetMatrixDirectory {
        path: Vec<u32>,
    },
    SetValue {
        path: Vec<u32>,
        value: WireValue,
    },
    Subscribe {
        path: Vec<u32>,
    },
    Unsubscribe {
        path: Vec<u32>,
    },
    MatrixConnect {
        path: Vec<u32>,
        target: u32,
        sources: Vec<u32>,
        operation: i32,
    },
    Invoke {
        path: Vec<u32>,
        invocation_id: i32,
        args: Vec<WireValue>,
    },
    Disconnect,
}

/// A provider the server offers (from its address book).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireProvider {
    pub id: u64,
    pub name: String,
    pub host: String,
    pub port: u16,
}

/// The address book as a tree of folders and providers, for the browser's
/// left-hand pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WireNode {
    Folder {
        name: String,
        children: Vec<WireNode>,
    },
    Provider(WireProvider),
}

/// Connection status of a provider, mirrored to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WireStatus {
    Connecting,
    Connected,
    Reconnecting { secs: u64, reason: String },
    Disconnected { reason: Option<String> },
    Error { message: String },
}

/// Client → server (JSON text frames).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ClientMsg {
    /// Sent first. `token` is omitted in open-LAN mode.
    Auth { token: Option<String> },
    /// Start mirroring a provider (by address-book id).
    OpenProvider { id: u64 },
    /// Stop mirroring a provider.
    CloseProvider { id: u64 },
    /// A command targeting a provider's connection.
    Command { id: u64, cmd: WireCommand },
}

/// Server → client (JSON text frames; documents arrive as binary frames).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ServerMsg {
    AuthOk {
        open_lan: bool,
    },
    AuthRejected,
    /// The provider list the client may open (flat).
    Providers {
        providers: Vec<WireProvider>,
    },
    /// The address book as folders + providers, for the left pane.
    AddressBook {
        nodes: Vec<WireNode>,
    },
    /// A provider's connection status changed.
    Status {
        id: u64,
        status: WireStatus,
    },
    /// The server rejected a control action (e.g. a read-only client tried to
    /// set a value).
    Denied {
        reason: String,
    },
}

impl ClientMsg {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ClientMsg serializes")
    }
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

impl ServerMsg {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ServerMsg serializes")
    }
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

// ---------------------------------------------------------------------------
// Binary document frames: [u64 provider_id LE][BER payload…]
// ---------------------------------------------------------------------------

/// Length of the provider-id prefix on a binary document frame.
const DOC_ID_LEN: usize = 8;

/// Build a binary document frame carrying `ber` for `provider_id`.
pub fn encode_doc_frame(provider_id: u64, ber: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DOC_ID_LEN + ber.len());
    out.extend_from_slice(&provider_id.to_le_bytes());
    out.extend_from_slice(ber);
    out
}

/// Split a binary document frame into `(provider_id, ber)`, or `None` if it is
/// too short to carry the id prefix.
pub fn decode_doc_frame(frame: &[u8]) -> Option<(u64, &[u8])> {
    if frame.len() < DOC_ID_LEN {
        return None;
    }
    let (id_bytes, ber) = frame.split_at(DOC_ID_LEN);
    let id = u64::from_le_bytes(id_bytes.try_into().ok()?);
    Some((id, ber))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_command_round_trips_through_json() {
        let msgs = [
            ClientMsg::Auth {
                token: Some("secret".into()),
            },
            ClientMsg::Auth { token: None },
            ClientMsg::OpenProvider { id: 7 },
            ClientMsg::CloseProvider { id: 7 },
            ClientMsg::Command {
                id: 7,
                cmd: WireCommand::GetDirectory {
                    path: vec![1, 3, 1],
                },
            },
            ClientMsg::Command {
                id: 7,
                cmd: WireCommand::SetValue {
                    path: vec![0, 1, 0],
                    value: WireValue::Int(42),
                },
            },
            ClientMsg::Command {
                id: 7,
                cmd: WireCommand::MatrixConnect {
                    path: vec![1, 3, 1],
                    target: 132,
                    sources: vec![129],
                    operation: 0,
                },
            },
            ClientMsg::Command {
                id: 7,
                cmd: WireCommand::Invoke {
                    path: vec![1, 5, 1],
                    invocation_id: 3,
                    args: vec![WireValue::Real(1.5), WireValue::Oct("AAEC".into())],
                },
            },
        ];
        for m in msgs {
            let json = m.to_json();
            assert_eq!(ClientMsg::from_json(&json).unwrap(), m, "json was {json}");
        }
    }

    #[test]
    fn server_msg_round_trips_through_json() {
        let msgs = [
            ServerMsg::AuthOk { open_lan: false },
            ServerMsg::AuthRejected,
            ServerMsg::Providers {
                providers: vec![WireProvider {
                    id: 1,
                    name: "Ruby".into(),
                    host: "10.0.0.2".into(),
                    port: 9000,
                }],
            },
            ServerMsg::Status {
                id: 1,
                status: WireStatus::Reconnecting {
                    secs: 4,
                    reason: "reset".into(),
                },
            },
            ServerMsg::Denied {
                reason: "read-only".into(),
            },
        ];
        for m in msgs {
            let json = m.to_json();
            assert_eq!(ServerMsg::from_json(&json).unwrap(), m, "json was {json}");
        }
    }

    #[test]
    fn doc_frame_round_trips() {
        let ber = [0x60u8, 0x80, 0x6b, 0x80, 0xff];
        let frame = encode_doc_frame(0x0102_0304_0506_0708, &ber);
        let (id, body) = decode_doc_frame(&frame).unwrap();
        assert_eq!(id, 0x0102_0304_0506_0708);
        assert_eq!(body, &ber);
        // Too short to hold an id prefix.
        assert!(decode_doc_frame(&[0, 1, 2]).is_none());
    }
}
