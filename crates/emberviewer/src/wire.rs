//! Conversions between the in-process types (`NetCommand`/`NetEvent`/`Value`) and
//! the WebSocket wire vocabulary in [`ember_web_proto`]. Shared by the server
//! (native, server mode) and the browser client (wasm) - both are this crate.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ember_proto::glow::{Real, Value};
use ember_web_proto::{WireCommand, WireStatus, WireValue};

use crate::net::{NetCommand, NetEvent};

/// `Value` → wire (octets base64-encoded).
pub fn value_to_wire(v: &Value) -> WireValue {
    match v {
        Value::Integer(i) => WireValue::Int(*i),
        Value::Real(r) => WireValue::Real(r.to_f64()),
        Value::String(s) => WireValue::Str(s.clone()),
        Value::Boolean(b) => WireValue::Bool(*b),
        Value::Octets(o) => WireValue::Oct(STANDARD.encode(o.as_ref())),
    }
}

/// Wire → `Value`. `None` if the octets base64 is malformed.
pub fn value_from_wire(w: &WireValue) -> Option<Value> {
    Some(match w {
        WireValue::Int(i) => Value::Integer(*i),
        WireValue::Real(f) => Value::Real(Real::from_f64(*f)),
        WireValue::Str(s) => Value::String(s.clone()),
        WireValue::Bool(b) => Value::Boolean(*b),
        WireValue::Oct(s) => Value::Octets(STANDARD.decode(s).ok()?.into()),
    })
}

/// `NetCommand` → wire.
pub fn command_to_wire(cmd: &NetCommand) -> WireCommand {
    match cmd {
        NetCommand::GetDirectory(p) => WireCommand::GetDirectory { path: p.clone() },
        NetCommand::GetMatrixDirectory(p) => WireCommand::GetMatrixDirectory { path: p.clone() },
        NetCommand::SetValue(p, v) => WireCommand::SetValue {
            path: p.clone(),
            value: value_to_wire(v),
        },
        NetCommand::Subscribe(p) => WireCommand::Subscribe { path: p.clone() },
        NetCommand::Unsubscribe(p) => WireCommand::Unsubscribe { path: p.clone() },
        NetCommand::MatrixConnect {
            path,
            target,
            sources,
            operation,
        } => WireCommand::MatrixConnect {
            path: path.clone(),
            target: *target,
            sources: sources.clone(),
            operation: *operation,
        },
        NetCommand::Invoke {
            path,
            invocation_id,
            args,
        } => WireCommand::Invoke {
            path: path.clone(),
            invocation_id: *invocation_id,
            args: args.iter().map(value_to_wire).collect(),
        },
        NetCommand::Disconnect => WireCommand::Disconnect,
    }
}

/// Wire → `NetCommand`. `None` if a contained value is malformed.
pub fn command_from_wire(w: WireCommand) -> Option<NetCommand> {
    Some(match w {
        WireCommand::GetDirectory { path } => NetCommand::GetDirectory(path),
        WireCommand::GetMatrixDirectory { path } => NetCommand::GetMatrixDirectory(path),
        WireCommand::SetValue { path, value } => {
            NetCommand::SetValue(path, value_from_wire(&value)?)
        }
        WireCommand::Subscribe { path } => NetCommand::Subscribe(path),
        WireCommand::Unsubscribe { path } => NetCommand::Unsubscribe(path),
        WireCommand::MatrixConnect {
            path,
            target,
            sources,
            operation,
        } => NetCommand::MatrixConnect {
            path,
            target,
            sources,
            operation,
        },
        WireCommand::Invoke {
            path,
            invocation_id,
            args,
        } => NetCommand::Invoke {
            path,
            invocation_id,
            args: args
                .iter()
                .map(value_from_wire)
                .collect::<Option<Vec<_>>>()?,
        },
        WireCommand::Disconnect => NetCommand::Disconnect,
    })
}

/// Map a `NetEvent` to a wire status, or `None` for documents (which travel as
/// binary frames, not status messages).
pub fn event_status(ev: &NetEvent) -> Option<WireStatus> {
    Some(match ev {
        NetEvent::Connected => WireStatus::Connected,
        NetEvent::Reconnecting {
            retry_in_secs,
            reason,
        } => WireStatus::Reconnecting {
            secs: *retry_in_secs,
            reason: reason.clone(),
        },
        NetEvent::Disconnected(reason) => WireStatus::Disconnected {
            reason: reason.clone(),
        },
        NetEvent::Error(message) => WireStatus::Error {
            message: message.clone(),
        },
        NetEvent::Document { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trips_through_wire() {
        let cmds = [
            NetCommand::GetDirectory(vec![1, 3, 1]),
            NetCommand::GetMatrixDirectory(vec![1, 3, 1]),
            NetCommand::SetValue(vec![0, 1], Value::Integer(-7)),
            NetCommand::SetValue(vec![0, 2], Value::Real(Real::from_f64(2.5))),
            NetCommand::SetValue(vec![0, 3], Value::String("hi".into())),
            NetCommand::SetValue(vec![0, 4], Value::Boolean(true)),
            NetCommand::SetValue(vec![0, 5], Value::Octets(vec![0, 1, 2, 255].into())),
            NetCommand::Subscribe(vec![0, 1]),
            NetCommand::Unsubscribe(vec![0, 1]),
            NetCommand::MatrixConnect {
                path: vec![1, 3, 1],
                target: 132,
                sources: vec![129, 130],
                operation: 0,
            },
            NetCommand::Invoke {
                path: vec![1, 5, 1],
                invocation_id: 9,
                args: vec![Value::Integer(1), Value::Octets(vec![9, 9].into())],
            },
            NetCommand::Disconnect,
        ];
        for cmd in &cmds {
            // NetCommand has no PartialEq; compare via the wire form after a
            // full round-trip, which also exercises every Value arm.
            let wire = command_to_wire(cmd);
            let back = command_from_wire(wire.clone()).expect("valid");
            assert_eq!(command_to_wire(&back), wire);
        }
    }

    #[test]
    fn malformed_base64_octets_rejected() {
        assert!(value_from_wire(&WireValue::Oct("not base64!!".into())).is_none());
    }

    #[test]
    fn document_events_have_no_status() {
        use std::sync::Arc;
        assert!(event_status(&NetEvent::Connected).is_some());
        assert!(event_status(&NetEvent::Document {
            roots: Arc::new(vec![dummy_root()]),
            raw: Arc::new(Vec::new()),
        })
        .is_none());
    }

    fn dummy_root() -> ember_proto::glow::Root {
        ember_proto::glow::Root::get_directory_at(&[])
    }
}
