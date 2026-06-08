//! S101 framing layer for Ember+.
//!
//! S101 packetizes a BER/Glow byte payload onto a TCP stream. Each frame is
//! delimited by [`BOF`]/[`EOF`], carries a small message header, and is protected
//! by a CRC. Payload bytes that collide with the control bytes are escaped.
//!
//! Constants verified against `libs101/Headers/s101/` in the Lawo/ember-plus repo.
//! The CRC is the standard CRC-16/X-25 (poly 0x1021 reflected, init 0xFFFF,
//! xorout 0xFFFF), whose residue is `0xF0B8` - matching `RxFrame.cs`.

use thiserror::Error;

/// Begin-of-frame delimiter.
pub const BOF: u8 = 0xFE;
/// End-of-frame delimiter.
pub const EOF: u8 = 0xFF;
/// Control-escape byte.
pub const CE: u8 = 0xFD;
/// XOR mask applied to an escaped byte.
pub const XOR: u8 = 0x20;
/// Bytes greater than or equal to this must be escaped.
pub const INVALID: u8 = 0xF8;

// ---- message header constants ----

/// Virtual device slot (single-slot providers use 0).
pub const SLOT: u8 = 0x00;
/// Message type: EmBER.
pub const MSG_EMBER: u8 = 0x0E;
/// S101 frame format version.
pub const VERSION: u8 = 0x01;
/// DTD identifier: Glow.
pub const DTD_GLOW: u8 = 0x01;

/// Glow DTD minor version reported in the app bytes (informational to providers).
pub const GLOW_DTD_MINOR: u8 = 31;
/// Glow DTD major version reported in the app bytes.
pub const GLOW_DTD_MAJOR: u8 = 2;

/// S101 command byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    /// Carries a (possibly partial) EmBER/Glow payload.
    Ember = 0x00,
    /// Request the peer to reply with [`Command::KeepAliveResponse`].
    KeepAliveRequest = 0x01,
    /// Reply to a keep-alive request.
    KeepAliveResponse = 0x02,
    /// Provider status notification.
    ProviderState = 0x03,
}

impl Command {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x00 => Some(Command::Ember),
            0x01 => Some(Command::KeepAliveRequest),
            0x02 => Some(Command::KeepAliveResponse),
            0x03 => Some(Command::ProviderState),
            _ => None,
        }
    }
}

/// Package flags for [`Command::Ember`] frames.
pub mod package_flag {
    /// First package of a multi-package payload.
    pub const FIRST: u8 = 0x80;
    /// Last package of a multi-package payload.
    pub const LAST: u8 = 0x40;
    /// Package carries no payload bytes.
    pub const EMPTY: u8 = 0x20;
    /// First and last - a self-contained payload.
    pub const SINGLE: u8 = FIRST | LAST;
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum S101Error {
    #[error("frame too short ({0} bytes)")]
    TooShort(usize),
    #[error("CRC mismatch: computed {computed:#06x}, received {received:#06x}")]
    CrcMismatch { computed: u16, received: u16 },
    #[error("dangling control-escape at end of frame")]
    DanglingEscape,
    #[error("unexpected message type {0:#04x} (expected EmBER 0x0E)")]
    BadMessageType(u8),
    #[error("unknown command byte {0:#04x}")]
    UnknownCommand(u8),
    #[error("malformed Ember header")]
    MalformedHeader,
}

/// CRC-16/X-25 over `data` (init 0xFFFF, reflected poly 0x1021, xorout 0xFFFF).
///
/// This is the value transmitted (little-endian) at the tail of each frame.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0x8408; // 0x8408 = bit-reversed 0x1021
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Append `byte`, escaping it if it collides with a control byte.
fn push_escaped(out: &mut Vec<u8>, byte: u8) {
    if byte >= INVALID {
        out.push(CE);
        out.push(byte ^ XOR);
    } else {
        out.push(byte);
    }
}

/// Undo S101 escaping. Returns the original byte sequence.
fn unescape(raw: &[u8]) -> Result<Vec<u8>, S101Error> {
    let mut out = Vec::with_capacity(raw.len());
    let mut iter = raw.iter();
    while let Some(&b) = iter.next() {
        if b == CE {
            match iter.next() {
                Some(&n) => out.push(n ^ XOR),
                None => return Err(S101Error::DanglingEscape),
            }
        } else {
            out.push(b);
        }
    }
    Ok(out)
}

/// Wrap a fully-built message (header + payload, unescaped, no CRC) into a
/// complete on-the-wire S101 frame: BOF, escaped(message + CRC), EOF.
fn frame_message(message: &[u8]) -> Vec<u8> {
    let crc = crc16(message);
    let mut out = Vec::with_capacity(message.len() + 8);
    out.push(BOF);
    for &b in message {
        push_escaped(&mut out, b);
    }
    // CRC transmitted little-endian, also escaped.
    push_escaped(&mut out, (crc & 0xFF) as u8);
    push_escaped(&mut out, (crc >> 8) as u8);
    out.push(EOF);
    out
}

/// Encode a keep-alive request frame.
pub fn encode_keepalive_request() -> Vec<u8> {
    frame_message(&[SLOT, MSG_EMBER, Command::KeepAliveRequest as u8, VERSION])
}

/// Encode a keep-alive response frame.
pub fn encode_keepalive_response() -> Vec<u8> {
    frame_message(&[SLOT, MSG_EMBER, Command::KeepAliveResponse as u8, VERSION])
}

/// Largest BER payload carried by a single Ember package. Larger payloads are
/// split across multiple packages using the First/Last flags.
pub const MAX_PACKAGE_PAYLOAD: usize = 1024;

/// Encode a BER/Glow payload into one or more S101 Ember frames.
///
/// Small payloads produce a single self-contained frame; larger payloads are
/// chunked with First/Last package flags so a peer can reassemble them.
pub fn encode_ember(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if payload.len() <= MAX_PACKAGE_PAYLOAD {
        out.extend_from_slice(&frame_ember_package(payload, package_flag::SINGLE));
        return out;
    }
    let chunks: Vec<&[u8]> = payload.chunks(MAX_PACKAGE_PAYLOAD).collect();
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.iter().enumerate() {
        let mut flags = 0u8;
        if i == 0 {
            flags |= package_flag::FIRST;
        }
        if i == last {
            flags |= package_flag::LAST;
        }
        out.extend_from_slice(&frame_ember_package(chunk, flags));
    }
    out
}

/// Build a single Ember-command S101 frame carrying `payload` with `flags`.
fn frame_ember_package(payload: &[u8], flags: u8) -> Vec<u8> {
    let mut message = Vec::with_capacity(payload.len() + 9);
    message.push(SLOT);
    message.push(MSG_EMBER);
    message.push(Command::Ember as u8);
    message.push(VERSION);
    message.push(flags);
    message.push(DTD_GLOW);
    message.push(0x02); // app-byte count
    message.push(GLOW_DTD_MINOR);
    message.push(GLOW_DTD_MAJOR);
    message.extend_from_slice(payload);
    frame_message(&message)
}

/// Something a [`FrameDecoder`] yields after consuming bytes from the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// A complete, reassembled BER/Glow payload.
    EmberPayload(Vec<u8>),
    /// Peer asked us to keep the connection alive - reply with a response.
    KeepAliveRequest,
    /// Peer answered our keep-alive request.
    KeepAliveResponse,
    /// Provider status bytes.
    ProviderState(Vec<u8>),
}

/// Streaming S101 deframer.
///
/// Feed arbitrary byte chunks via [`FrameDecoder::push`]; it returns any
/// complete messages found, transparently unescaping, checking CRCs, and
/// reassembling multi-package Ember payloads.
#[derive(Default)]
pub struct FrameDecoder {
    /// Raw (still-escaped) bytes of the frame currently being collected.
    frame: Vec<u8>,
    in_frame: bool,
    /// BER payload accumulated across First..Last packages.
    reassembly: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume `bytes`, returning every complete message decoded so far.
    ///
    /// A CRC or framing error on one frame is surfaced but does not poison the
    /// decoder - subsequent frames still decode.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Incoming, S101Error>> {
        let mut results = Vec::new();
        for &b in bytes {
            match b {
                BOF => {
                    // Start (or restart) a frame.
                    self.in_frame = true;
                    self.frame.clear();
                }
                EOF if self.in_frame => {
                    self.in_frame = false;
                    let raw = std::mem::take(&mut self.frame);
                    match self.process_raw(&raw) {
                        Ok(Some(msg)) => results.push(Ok(msg)),
                        Ok(None) => {} // partial package; keep accumulating
                        Err(e) => results.push(Err(e)),
                    }
                }
                _ if self.in_frame => self.frame.push(b),
                _ => {} // bytes outside any frame are ignored
            }
        }
        results
    }

    fn process_raw(&mut self, raw: &[u8]) -> Result<Option<Incoming>, S101Error> {
        let unescaped = unescape(raw)?;
        if unescaped.len() < 2 {
            return Err(S101Error::TooShort(unescaped.len()));
        }
        let (message, crc_bytes) = unescaped.split_at(unescaped.len() - 2);
        let received = u16::from_le_bytes([crc_bytes[0], crc_bytes[1]]);
        let computed = crc16(message);
        if computed != received {
            return Err(S101Error::CrcMismatch { computed, received });
        }
        self.parse_message(message)
    }

    fn parse_message(&mut self, msg: &[u8]) -> Result<Option<Incoming>, S101Error> {
        // [slot, msgType, command, version, ...]
        if msg.len() < 4 {
            return Err(S101Error::TooShort(msg.len()));
        }
        if msg[1] != MSG_EMBER {
            return Err(S101Error::BadMessageType(msg[1]));
        }
        let command = Command::from_u8(msg[2]).ok_or(S101Error::UnknownCommand(msg[2]))?;
        match command {
            Command::KeepAliveRequest => Ok(Some(Incoming::KeepAliveRequest)),
            Command::KeepAliveResponse => Ok(Some(Incoming::KeepAliveResponse)),
            Command::ProviderState => Ok(Some(Incoming::ProviderState(msg[3..].to_vec()))),
            Command::Ember => self.parse_ember(msg),
        }
    }

    fn parse_ember(&mut self, msg: &[u8]) -> Result<Option<Incoming>, S101Error> {
        // [slot, msgType, command, version, flags, dtd, appCount, app.., payload..]
        if msg.len() < 7 {
            return Err(S101Error::MalformedHeader);
        }
        let flags = msg[4];
        let app_count = msg[6] as usize;
        let payload_start = 7 + app_count;
        if msg.len() < payload_start {
            return Err(S101Error::MalformedHeader);
        }
        let payload = &msg[payload_start..];

        if flags & package_flag::FIRST != 0 {
            self.reassembly.clear();
        }
        if flags & package_flag::EMPTY == 0 {
            self.reassembly.extend_from_slice(payload);
        }
        if flags & package_flag::LAST != 0 {
            let complete = std::mem::take(&mut self.reassembly);
            Ok(Some(Incoming::EmberPayload(complete)))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_check_value() {
        // CRC-16/X-25 check value for the ASCII string "123456789".
        assert_eq!(crc16(b"123456789"), 0x906E);
    }

    #[test]
    fn crc16_residue() {
        // Running the (init 0xFFFF, no xorout) register over message+CRC must
        // land on the X-25 residue 0xF0B8 - the check RxFrame.cs performs.
        let msg = [SLOT, MSG_EMBER, Command::KeepAliveRequest as u8, VERSION];
        let crc = crc16(&msg);
        let mut buf = msg.to_vec();
        buf.extend_from_slice(&crc.to_le_bytes());
        let mut reg: u16 = 0xFFFF;
        for &b in &buf {
            reg ^= b as u16;
            for _ in 0..8 {
                reg = if reg & 1 != 0 {
                    (reg >> 1) ^ 0x8408
                } else {
                    reg >> 1
                };
            }
        }
        assert_eq!(reg, 0xF0B8);
    }

    #[test]
    fn escape_roundtrip_high_bytes() {
        let original = [0x00, 0xFE, 0xFF, 0xFD, 0xF8, 0x12, 0xF0];
        let mut escaped = Vec::new();
        for &b in &original {
            push_escaped(&mut escaped, b);
        }
        // Every byte >= 0xF8 must have been escaped.
        assert!(escaped.len() > original.len());
        assert_eq!(unescape(&escaped).unwrap(), original);
    }

    #[test]
    fn unescape_dangling_escape_errors() {
        assert_eq!(unescape(&[0x01, CE]), Err(S101Error::DanglingEscape));
    }

    #[test]
    fn keepalive_request_roundtrips() {
        let frame = encode_keepalive_request();
        assert_eq!(frame.first(), Some(&BOF));
        assert_eq!(frame.last(), Some(&EOF));
        let mut dec = FrameDecoder::new();
        let out = dec.push(&frame);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref().unwrap(), &Incoming::KeepAliveRequest);
    }

    #[test]
    fn ember_single_frame_roundtrips() {
        let payload = b"\x60\x03\x02\x01\x20"; // arbitrary BER-ish bytes
        let frame = encode_ember(payload);
        let mut dec = FrameDecoder::new();
        let out = dec.push(&frame);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].as_ref().unwrap(),
            &Incoming::EmberPayload(payload.to_vec())
        );
    }

    #[test]
    fn ember_payload_with_escapable_bytes() {
        // Payload containing control bytes must survive framing intact.
        let payload: Vec<u8> = (0u16..=255).map(|b| b as u8).collect();
        let frame = encode_ember(&payload);
        let mut dec = FrameDecoder::new();
        let out = dec.push(&frame);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref().unwrap(), &Incoming::EmberPayload(payload));
    }

    #[test]
    fn ember_multi_package_reassembles() {
        let payload: Vec<u8> = (0..(MAX_PACKAGE_PAYLOAD * 2 + 50))
            .map(|i| (i % 251) as u8)
            .collect();
        let frames = encode_ember(&payload);
        let mut dec = FrameDecoder::new();
        let out = dec.push(&frames);
        // Only the final (LAST) package yields a message.
        let msgs: Vec<_> = out.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], Incoming::EmberPayload(payload));
    }

    #[test]
    fn decoder_handles_split_across_pushes() {
        let frame = encode_keepalive_response();
        let (a, b) = frame.split_at(frame.len() / 2);
        let mut dec = FrameDecoder::new();
        assert!(dec.push(a).is_empty());
        let out = dec.push(b);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref().unwrap(), &Incoming::KeepAliveResponse);
    }

    #[test]
    fn corrupted_crc_is_reported() {
        let mut frame = encode_keepalive_request();
        // Flip a low bit of the msgType byte so it stays a normal (non-delimiter,
        // non-escape) byte but changes the message - forcing a CRC mismatch.
        frame[2] ^= 0x01;
        let mut dec = FrameDecoder::new();
        let out = dec.push(&frame);
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0],
            Err(S101Error::CrcMismatch { .. }) | Err(S101Error::BadMessageType(_))
        ));
    }
}
