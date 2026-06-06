//! The Glow schema: Ember+ tree types, encoded with BER via `rasn`.
//!
//! Transcribed from `GlowDtd.asn1` (Lawo/ember-plus). The module is
//! `DEFINITIONS EXPLICIT TAGS`, so every context tag `[n]` is an *explicit*
//! wrapper, while the `[APPLICATION n] IMPLICIT` outer tags replace the
//! universal tag. Tagging here is verified against frames captured from a live
//! `node-emberplus` provider (see the `fixtures` tests).
//!
//! Wire shape of a Glow message (from a real capture):
//! ```text
//! [APPLICATION 0] Root
//!   [APPLICATION 11] RootElementCollection   (SEQUENCE OF [0] RootElement)
//!     [0] RootElement
//!       [APPLICATION 10] QualifiedNode  /  [APPLICATION 2] Command  / ...
//! ```

use rasn::prelude::*;

/// `Integer32` in the DTD.
pub type Integer32 = i32;
/// `Integer64` in the DTD.
pub type Integer64 = i64;
/// `EmberString ::= UTF8String`.
pub type EmberString = Utf8String;

// ---------------------------------------------------------------------------
// Enumerations (encoded as plain INTEGER per the DTD)
// ---------------------------------------------------------------------------

/// `CommandType` values.
pub mod command_type {
    pub const SUBSCRIBE: i32 = 30;
    pub const UNSUBSCRIBE: i32 = 31;
    pub const GET_DIRECTORY: i32 = 32;
    pub const INVOKE: i32 = 33;
}

/// `ParameterType` values.
pub mod parameter_type {
    pub const INTEGER: i32 = 1;
    pub const REAL: i32 = 2;
    pub const STRING: i32 = 3;
    pub const BOOLEAN: i32 = 4;
    pub const TRIGGER: i32 = 5;
    pub const ENUM: i32 = 6;
    pub const OCTETS: i32 = 7;
}

/// `ParameterAccess` values.
pub mod access {
    pub const NONE: i32 = 0;
    pub const READ: i32 = 1;
    pub const WRITE: i32 = 2;
    pub const READ_WRITE: i32 = 3;
}

/// `FieldFlags` (dirFieldMask) values.
pub mod field_flags {
    pub const ALL: i32 = -1;
    pub const DEFAULT: i32 = 0;
    pub const IDENTIFIER: i32 = 1;
    pub const DESCRIPTION: i32 = 2;
    pub const TREE: i32 = 3;
    pub const VALUE: i32 = 4;
    pub const CONNECTIONS: i32 = 5;
}

// ---------------------------------------------------------------------------
// RELATIVE-OID — Ember+ paths.
//
// rasn has no RELATIVE-OID type, so we delegate to an OctetString carrying the
// universal RELATIVE-OID tag (13) and do the base-128 sub-identifier codec
// ourselves. A path like `1.3.2` is a list of u32 arcs.
// ---------------------------------------------------------------------------

/// An Ember+ relative path (e.g. `1.3.2`), encoded on the wire as RELATIVE-OID.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq, Eq, Hash)]
#[rasn(delegate, tag(universal, 13))]
pub struct RelativeOid(pub OctetString);

impl RelativeOid {
    /// Build from a list of integer arcs.
    pub fn from_arcs(arcs: &[u32]) -> Self {
        let mut out = Vec::new();
        for &arc in arcs {
            encode_base128(arc, &mut out);
        }
        RelativeOid(out.into())
    }

    /// Decode the arcs from the base-128 content octets.
    pub fn arcs(&self) -> Vec<u32> {
        let mut arcs = Vec::new();
        let mut value: u32 = 0;
        for &b in self.0.iter() {
            value = (value << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                arcs.push(value);
                value = 0;
            }
        }
        arcs
    }
}

/// Append `value` as base-128 big-endian sub-identifier octets (high bit set on
/// all but the final octet).
fn encode_base128(value: u32, out: &mut Vec<u8>) {
    let mut stack = [0u8; 5];
    let mut n = 0;
    let mut v = value;
    loop {
        stack[n] = (v & 0x7F) as u8;
        n += 1;
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        let mut byte = stack[i];
        if i != 0 {
            byte |= 0x80;
        }
        out.push(byte);
    }
}

// ---------------------------------------------------------------------------
// REAL — ASN.1 universal tag 9.
//
// rasn's BER codec does not support REAL, so (as with RELATIVE-OID) we delegate
// to an OctetString carrying tag 9 and implement the content codec (X.690 §8.5)
// ourselves. We only ever emit the binary base-2 form; we decode binary, the
// special values, and the decimal form on input.
// ---------------------------------------------------------------------------

/// An ASN.1 REAL value, stored as its BER content octets.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq, Eq)]
#[rasn(delegate, tag(universal, 9))]
pub struct Real(pub OctetString);

impl Real {
    /// Encode an IEEE-754 double into REAL content octets (binary base-2 form).
    pub fn from_f64(value: f64) -> Self {
        Real(encode_real(value).into())
    }

    /// Decode the REAL content octets back into a double.
    pub fn to_f64(&self) -> f64 {
        decode_real(&self.0)
    }
}

impl From<f64> for Real {
    fn from(v: f64) -> Self {
        Real::from_f64(v)
    }
}

/// Minimal big-endian unsigned octets (at least one byte).
fn unsigned_min_bytes(mut v: u64) -> Vec<u8> {
    if v == 0 {
        return vec![0];
    }
    let mut buf = Vec::new();
    while v != 0 {
        buf.push((v & 0xFF) as u8);
        v >>= 8;
    }
    buf.reverse();
    buf
}

/// Minimal two's-complement octets for a signed exponent (at least one byte).
fn signed_min_bytes(v: i64) -> Vec<u8> {
    let mut buf = v.to_be_bytes().to_vec();
    // Trim redundant leading 0x00 / 0xFF while preserving the sign bit.
    while buf.len() > 1 {
        let b0 = buf[0];
        let b1 = buf[1];
        if (b0 == 0x00 && b1 & 0x80 == 0) || (b0 == 0xFF && b1 & 0x80 != 0) {
            buf.remove(0);
        } else {
            break;
        }
    }
    buf
}

/// Encode `value` as ASN.1 REAL content octets, using the Ember+ convention.
///
/// Mirrors libember/node-emberplus: the exponent stored is the IEEE-754 binary
/// exponent (`bias 1023`), and the mantissa is the 53-bit significand (implicit
/// leading 1 included). Trailing zero bits are stripped for compactness — the
/// decoder re-normalises, so this is lossless.
fn encode_real(value: f64) -> Vec<u8> {
    if value == 0.0 {
        // +0.0 → empty content; -0.0 → special "minus zero".
        return if value.is_sign_negative() {
            vec![0x43]
        } else {
            Vec::new()
        };
    }
    if value.is_nan() {
        return vec![0x42];
    }
    if value.is_infinite() {
        return vec![if value < 0.0 { 0x41 } else { 0x40 }];
    }

    let bits = value.to_bits();
    let sign_negative = bits >> 63 == 1;
    let exponent = (((bits >> 52) & 0x7FF) as i64) - 1023;
    // Significand with the implicit leading 1 bit.
    let mut significand = (bits & 0x000F_FFFF_FFFF_FFFF) | 0x0010_0000_0000_0000;
    // Strip trailing zero bytes then bits (decoder re-normalises on the way in).
    while significand & 0xFF == 0 {
        significand >>= 8;
    }
    while significand & 0x01 == 0 {
        significand >>= 1;
    }

    let exp_bytes = signed_min_bytes(exponent);
    let mant_bytes = unsigned_min_bytes(significand);

    // Preamble: binary(0x80) | sign | base-2(00) | scale-0(00) | exp-length-1.
    let mut preamble = 0x80u8;
    if sign_negative {
        preamble |= 0x40;
    }
    preamble |= ((exp_bytes.len() - 1) & 0x03) as u8;

    let mut out = vec![preamble];
    out.extend_from_slice(&exp_bytes);
    out.extend_from_slice(&mant_bytes);
    out
}

/// Decode ASN.1 REAL content octets into a double, using the Ember+ convention.
///
/// Faithful port of `libember/.../ber/traits/Real.hpp`: the exponent is the
/// IEEE binary exponent and the mantissa is re-normalised into the 52-bit
/// fraction field before reconstructing the IEEE bit pattern.
fn decode_real(content: &[u8]) -> f64 {
    if content.is_empty() {
        return 0.0;
    }
    let preamble = content[0];
    if content.len() == 1 {
        match preamble {
            0x40 => return f64::INFINITY,
            0x41 => return f64::NEG_INFINITY,
            0x42 => return f64::NAN,
            0x43 => return -0.0,
            _ => {}
        }
    }
    // Decimal (base-10) form has bit 8 clear; handle it best-effort.
    if preamble & 0x80 == 0 {
        return std::str::from_utf8(&content[1..])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0.0);
    }

    let sign_negative = preamble & 0x40 != 0;
    let exponent_length = 1 + (preamble & 0x03) as usize;
    let mantissa_shift = ((preamble >> 2) & 0x03) as u32;
    if content.len() < 1 + exponent_length {
        return 0.0;
    }

    let exp_slice = &content[1..1 + exponent_length];
    let mut exponent: i64 = if exp_slice[0] & 0x80 != 0 { -1 } else { 0 };
    for &b in exp_slice {
        exponent = (exponent << 8) | b as i64;
    }

    let mut mantissa: u64 = 0;
    for &b in &content[1 + exponent_length..] {
        mantissa = (mantissa << 8) | b as u64;
    }
    mantissa <<= mantissa_shift;

    if mantissa != 0 {
        while mantissa & 0x7FFF_F000_0000_0000 == 0 {
            mantissa <<= 8;
        }
        while mantissa & 0x7FF0_0000_0000_0000 == 0 {
            mantissa <<= 1;
        }
    }
    mantissa &= 0x000F_FFFF_FFFF_FFFF;

    let mut bits = (((exponent + 1023) as u64) << 52) | mantissa;
    if sign_negative {
        bits |= 0x8000_0000_0000_0000;
    }
    f64::from_bits(bits)
}

// ---------------------------------------------------------------------------
// Value
// ---------------------------------------------------------------------------

/// `Value ::= CHOICE { integer, real, string, boolean, octets }`.
///
/// Distinguished by the alternatives' universal tags, so no explicit tagging.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(choice)]
pub enum Value {
    Integer(Integer64),
    Real(Real),
    String(EmberString),
    Boolean(bool),
    Octets(OctetString),
}

/// `MinMax ::= CHOICE { integer, real }`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(choice)]
pub enum MinMax {
    Integer(Integer64),
    Real(Real),
}

// ---------------------------------------------------------------------------
// StringIntegerPair / Collection, StreamDescription
// ---------------------------------------------------------------------------

/// `StringIntegerPair ::= [APPLICATION 7] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 7))]
pub struct StringIntegerPair {
    #[rasn(tag(explicit(0)))]
    pub entry_string: EmberString,
    #[rasn(tag(explicit(1)))]
    pub entry_integer: Integer32,
}

/// Explicit `[0]` wrapper for entries of `StringIntegerCollection`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(explicit(0)))]
pub struct StringIntegerEntry(pub StringIntegerPair);

/// `StringIntegerCollection ::= [APPLICATION 8] IMPLICIT SEQUENCE OF [0] StringIntegerPair`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(application, 8))]
pub struct StringIntegerCollection(pub Vec<StringIntegerEntry>);

/// `StreamDescription ::= [APPLICATION 12] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 12))]
pub struct StreamDescription {
    #[rasn(tag(explicit(0)))]
    pub format: Integer32,
    #[rasn(tag(explicit(1)))]
    pub offset: Integer32,
}

// ---------------------------------------------------------------------------
// Parameter
// ---------------------------------------------------------------------------

/// `ParameterContents ::= SET { ... }` — all fields optional.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq, Default)]
#[rasn(set)]
pub struct ParameterContents {
    #[rasn(tag(explicit(0)))]
    pub identifier: Option<EmberString>,
    #[rasn(tag(explicit(1)))]
    pub description: Option<EmberString>,
    // NB: named `value_` not `value` — a field named `value` collides with an
    // internal binding in rasn's generated SET decoder.
    #[rasn(tag(explicit(2)))]
    pub value_: Option<Value>,
    #[rasn(tag(explicit(3)))]
    pub minimum: Option<MinMax>,
    #[rasn(tag(explicit(4)))]
    pub maximum: Option<MinMax>,
    #[rasn(tag(explicit(5)))]
    pub access: Option<Integer32>,
    #[rasn(tag(explicit(6)))]
    pub format: Option<EmberString>,
    #[rasn(tag(explicit(7)))]
    pub enumeration: Option<EmberString>,
    #[rasn(tag(explicit(8)))]
    pub factor: Option<Integer32>,
    #[rasn(tag(explicit(9)))]
    pub is_online: Option<bool>,
    #[rasn(tag(explicit(10)))]
    pub formula: Option<EmberString>,
    #[rasn(tag(explicit(11)))]
    pub step: Option<Integer32>,
    #[rasn(tag(explicit(12)))]
    pub default: Option<Value>,
    #[rasn(tag(explicit(13)))]
    pub r#type: Option<Integer32>,
    #[rasn(tag(explicit(14)))]
    pub stream_identifier: Option<Integer32>,
    #[rasn(tag(explicit(15)))]
    pub enum_map: Option<StringIntegerCollection>,
    #[rasn(tag(explicit(16)))]
    pub stream_descriptor: Option<StreamDescription>,
    #[rasn(tag(explicit(17)))]
    pub schema_identifiers: Option<EmberString>,
}

/// `Parameter ::= [APPLICATION 1] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 1))]
pub struct Parameter {
    #[rasn(tag(explicit(0)))]
    pub number: Integer32,
    #[rasn(tag(explicit(1)))]
    pub contents: Option<ParameterContents>,
    #[rasn(tag(explicit(2)))]
    pub children: Option<ElementCollection>,
}

/// `QualifiedParameter ::= [APPLICATION 9] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 9))]
pub struct QualifiedParameter {
    #[rasn(tag(explicit(0)))]
    pub path: RelativeOid,
    #[rasn(tag(explicit(1)))]
    pub contents: Option<ParameterContents>,
    #[rasn(tag(explicit(2)))]
    pub children: Option<ElementCollection>,
}

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

/// `NodeContents ::= SET { ... }` — all fields optional.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq, Default)]
#[rasn(set)]
pub struct NodeContents {
    #[rasn(tag(explicit(0)))]
    pub identifier: Option<EmberString>,
    #[rasn(tag(explicit(1)))]
    pub description: Option<EmberString>,
    #[rasn(tag(explicit(2)))]
    pub is_root: Option<bool>,
    #[rasn(tag(explicit(3)))]
    pub is_online: Option<bool>,
    #[rasn(tag(explicit(4)))]
    pub schema_identifiers: Option<EmberString>,
}

/// `Node ::= [APPLICATION 3] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 3))]
pub struct Node {
    #[rasn(tag(explicit(0)))]
    pub number: Integer32,
    #[rasn(tag(explicit(1)))]
    pub contents: Option<NodeContents>,
    #[rasn(tag(explicit(2)))]
    pub children: Option<ElementCollection>,
}

/// `QualifiedNode ::= [APPLICATION 10] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 10))]
pub struct QualifiedNode {
    #[rasn(tag(explicit(0)))]
    pub path: RelativeOid,
    #[rasn(tag(explicit(1)))]
    pub contents: Option<NodeContents>,
    #[rasn(tag(explicit(2)))]
    pub children: Option<ElementCollection>,
}

// ---------------------------------------------------------------------------
// Command (+ invocation)
// ---------------------------------------------------------------------------

/// `Invocation ::= [APPLICATION 22] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 22))]
pub struct Invocation {
    #[rasn(tag(explicit(0)))]
    pub invocation_id: Option<Integer32>,
    #[rasn(tag(explicit(1)))]
    pub arguments: Option<ValueTuple>,
}

/// Explicit `[0]` wrapper for tuple values.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(explicit(0)))]
pub struct TupleValue(pub Value);

/// `Tuple ::= SEQUENCE OF [0] Value` (used by invocations/results).
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate)]
pub struct ValueTuple(pub Vec<TupleValue>);

/// `Command`'s inline `options` CHOICE.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(choice)]
pub enum CommandOptions {
    #[rasn(tag(explicit(1)))]
    DirFieldMask(Integer32),
    #[rasn(tag(explicit(2)))]
    Invocation(Invocation),
}

/// `Command ::= [APPLICATION 2] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 2))]
pub struct Command {
    #[rasn(tag(explicit(0)))]
    pub number: Integer32,
    pub options: Option<CommandOptions>,
}

impl Command {
    /// A `getDirectory` command (optionally with a field mask).
    pub fn get_directory(field_mask: Option<Integer32>) -> Self {
        Command {
            number: command_type::GET_DIRECTORY,
            options: field_mask.map(CommandOptions::DirFieldMask),
        }
    }

    /// A bare command carrying only a command number (subscribe/unsubscribe).
    pub fn bare(number: Integer32) -> Self {
        Command {
            number,
            options: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix / Function — modelled enough to round-trip; refined in Phase 4.
// ---------------------------------------------------------------------------

/// `Matrix ::= [APPLICATION 13] IMPLICIT SEQUENCE` (subset).
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 13))]
pub struct Matrix {
    #[rasn(tag(explicit(0)))]
    pub number: Integer32,
    #[rasn(tag(explicit(1)))]
    pub contents: Option<Any>,
    #[rasn(tag(explicit(2)))]
    pub children: Option<ElementCollection>,
}

/// `QualifiedMatrix ::= [APPLICATION 17] IMPLICIT SEQUENCE` (subset).
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 17))]
pub struct QualifiedMatrix {
    #[rasn(tag(explicit(0)))]
    pub path: RelativeOid,
    #[rasn(tag(explicit(1)))]
    pub contents: Option<Any>,
    #[rasn(tag(explicit(2)))]
    pub children: Option<ElementCollection>,
}

/// `Function ::= [APPLICATION 19] IMPLICIT SEQUENCE` (subset).
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 19))]
pub struct Function {
    #[rasn(tag(explicit(0)))]
    pub number: Integer32,
    #[rasn(tag(explicit(1)))]
    pub contents: Option<Any>,
    #[rasn(tag(explicit(2)))]
    pub children: Option<ElementCollection>,
}

/// `QualifiedFunction ::= [APPLICATION 20] IMPLICIT SEQUENCE` (subset).
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 20))]
pub struct QualifiedFunction {
    #[rasn(tag(explicit(0)))]
    pub path: RelativeOid,
    #[rasn(tag(explicit(1)))]
    pub contents: Option<Any>,
    #[rasn(tag(explicit(2)))]
    pub children: Option<ElementCollection>,
}

// ---------------------------------------------------------------------------
// Element collections and the Root document wrapper
// ---------------------------------------------------------------------------

/// `Element ::= CHOICE { parameter, node, command, matrix, function }`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(choice)]
pub enum Element {
    Parameter(Parameter),
    Node(Node),
    Command(Command),
    Matrix(Matrix),
    Function(Function),
}

/// Explicit `[0]` wrapper for entries of `ElementCollection`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(explicit(0)))]
pub struct ElementEntry(pub Element);

/// `ElementCollection ::= [APPLICATION 4] IMPLICIT SEQUENCE OF [0] Element`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(application, 4))]
pub struct ElementCollection(pub Vec<ElementEntry>);

/// `RootElement ::= CHOICE { element, qualifiedParameter, qualifiedNode, qualifiedMatrix, qualifiedFunction }`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(choice)]
pub enum RootElement {
    Element(Element),
    QualifiedParameter(QualifiedParameter),
    QualifiedNode(QualifiedNode),
    QualifiedMatrix(QualifiedMatrix),
    QualifiedFunction(QualifiedFunction),
}

/// Explicit `[0]` wrapper for entries of `RootElementCollection`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(explicit(0)))]
pub struct RootElementEntry(pub RootElement);

/// `RootElementCollection ::= [APPLICATION 11] IMPLICIT SEQUENCE OF [0] RootElement`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(application, 11))]
pub struct RootElementCollection(pub Vec<RootElementEntry>);

/// Explicit `[0]` wrapper for stream entries.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(explicit(0)))]
pub struct StreamEntryWrap(pub StreamEntry);

/// `StreamEntry ::= [APPLICATION 5] IMPLICIT SEQUENCE`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 5))]
pub struct StreamEntry {
    #[rasn(tag(explicit(0)))]
    pub stream_identifier: Integer32,
    #[rasn(tag(explicit(1)))]
    pub stream_value: Value,
}

/// `StreamCollection ::= [APPLICATION 6] IMPLICIT SEQUENCE OF [0] StreamEntry`.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(delegate, tag(application, 6))]
pub struct StreamCollection(pub Vec<StreamEntryWrap>);

/// `InvocationResult ::= [APPLICATION 23] IMPLICIT SEQUENCE` (subset).
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(tag(application, 23))]
pub struct InvocationResult {
    #[rasn(tag(explicit(0)))]
    pub invocation_id: Option<Integer32>,
    #[rasn(tag(explicit(1)))]
    pub success: Option<bool>,
    #[rasn(tag(explicit(2)))]
    pub result: Option<ValueTuple>,
}

/// `Root ::= [APPLICATION 0] CHOICE { ... }` — the outermost document wrapper.
///
/// The `[APPLICATION 0]` tag is explicit (it wraps a CHOICE). Every Glow message
/// on the wire begins here.
#[derive(AsnType, Decode, Encode, Debug, Clone, PartialEq)]
#[rasn(choice, tag(explicit(application, 0)))]
pub enum Root {
    Elements(RootElementCollection),
    Streams(StreamCollection),
    InvocationResult(InvocationResult),
}

impl Root {
    /// Wrap a single root element into a `Root` document.
    pub fn from_element(elem: RootElement) -> Self {
        Root::Elements(RootElementCollection(vec![RootElementEntry(elem)]))
    }

    /// Build a root `getDirectory` request document.
    pub fn root_get_directory() -> Self {
        Root::from_element(RootElement::Element(Element::Command(Command::get_directory(
            None,
        ))))
    }

    /// Build a `getDirectory` request for the node at `path`.
    ///
    /// An empty path requests the root; otherwise we send a `QualifiedNode` at
    /// `path` whose `children` carry the `getDirectory` command — the addressing
    /// scheme captured from real providers.
    pub fn get_directory_at(path: &[u32]) -> Self {
        let command = Element::Command(Command::get_directory(Some(field_flags::ALL)));
        if path.is_empty() {
            return Root::from_element(RootElement::Element(command));
        }
        let qn = QualifiedNode {
            path: RelativeOid::from_arcs(path),
            contents: None,
            children: Some(ElementCollection(vec![ElementEntry(command)])),
        };
        Root::from_element(RootElement::QualifiedNode(qn))
    }

    /// Subscribe to value changes of the parameter at `path`.
    pub fn subscribe_at(path: &[u32]) -> Self {
        Root::command_on_parameter(path, command_type::SUBSCRIBE)
    }

    /// Unsubscribe from value changes of the parameter at `path`.
    pub fn unsubscribe_at(path: &[u32]) -> Self {
        Root::command_on_parameter(path, command_type::UNSUBSCRIBE)
    }

    /// Address the parameter at `path` and place `command_number` in its children.
    fn command_on_parameter(path: &[u32], command_number: Integer32) -> Self {
        let command = Element::Command(Command::bare(command_number));
        let qp = QualifiedParameter {
            path: RelativeOid::from_arcs(path),
            contents: None,
            children: Some(ElementCollection(vec![ElementEntry(command)])),
        };
        Root::from_element(RootElement::QualifiedParameter(qp))
    }

    /// Build a request that sets the parameter at `path` to `value`.
    ///
    /// Per Ember+, a set is just the parameter sent back carrying only the new
    /// `value`; the provider applies it and echoes the resulting value.
    pub fn set_value_at(path: &[u32], value: Value) -> Self {
        let contents = ParameterContents {
            value_: Some(value),
            ..Default::default()
        };
        let qp = QualifiedParameter {
            path: RelativeOid::from_arcs(path),
            contents: Some(contents),
            children: None,
        };
        Root::from_element(RootElement::QualifiedParameter(qp))
    }
}

/// Encode a `Root` document to BER bytes.
pub fn encode_root(root: &Root) -> Result<Vec<u8>, rasn::error::EncodeError> {
    rasn::ber::encode(root)
}

/// Decode a `Root` document from BER bytes.
pub fn decode_root(bytes: &[u8]) -> Result<Root, rasn::error::DecodeError> {
    rasn::ber::decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_roundtrips() {
        let cases = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            3.14159,
            2.5,
            0.1,
            -0.001,
            1e10,
            1e-10,
            f64::MAX,
            f64::MIN_POSITIVE,
            123456789.0,
        ];
        for &v in &cases {
            let encoded = Real::from_f64(v);
            let back = encoded.to_f64();
            if v == 0.0 {
                assert_eq!(back, 0.0, "zero case");
            } else {
                assert!(
                    (back - v).abs() <= v.abs() * 1e-12,
                    "roundtrip failed: {v} -> {back}"
                );
            }
        }
    }

    #[test]
    fn real_special_values() {
        assert!(Real::from_f64(f64::INFINITY).to_f64().is_infinite());
        assert!(Real::from_f64(f64::NEG_INFINITY).to_f64() < 0.0);
        assert!(Real::from_f64(f64::NAN).to_f64().is_nan());
    }

    #[test]
    fn relative_oid_roundtrips() {
        for arcs in [vec![0], vec![1, 3, 2], vec![0, 1, 4], vec![128, 300, 16384]] {
            let oid = RelativeOid::from_arcs(&arcs);
            assert_eq!(oid.arcs(), arcs);
        }
    }

    #[test]
    fn subscribe_request_roundtrips() {
        let req = Root::subscribe_at(&[0, 1, 0]);
        let bytes = encode_root(&req).unwrap();
        let back = decode_root(&bytes).unwrap();
        let Root::Elements(coll) = back else {
            panic!("expected elements")
        };
        let RootElementEntry(RootElement::QualifiedParameter(qp)) = &coll.0[0] else {
            panic!("expected qualified parameter")
        };
        assert_eq!(qp.path.arcs(), vec![0, 1, 0]);
        let ElementEntry(Element::Command(cmd)) = &qp.children.as_ref().unwrap().0[0] else {
            panic!("expected child command")
        };
        assert_eq!(cmd.number, command_type::SUBSCRIBE);
    }

    #[test]
    fn value_choice_roundtrips() {
        for v in [
            Value::Integer(42),
            Value::Integer(-7),
            Value::String("hi".into()),
            Value::Boolean(true),
            Value::Real(Real::from_f64(2.5)),
        ] {
            let bytes = rasn::ber::encode(&v).unwrap();
            let back: Value = rasn::ber::decode(&bytes).unwrap();
            assert_eq!(v, back);
        }
    }
}
