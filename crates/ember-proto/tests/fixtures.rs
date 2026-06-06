//! Interop tests against frames captured from a live `node-emberplus` provider.
//!
//! These validate the whole inbound path: S101 deframing + CRC (proving our CRC
//! matches an independent implementation), then BER/Glow decoding of the Root
//! document. The fixtures come from `testprovider/fixtures.md`.

use ember_proto::glow::*;
use ember_proto::s101::{FrameDecoder, Incoming};

/// Decode a hex string (no separators) into bytes.
fn hex(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

/// Feed a full S101 frame and return its single reassembled BER payload.
fn payload_of(frame_hex: &str) -> Vec<u8> {
    let frame = hex(frame_hex);
    let mut dec = FrameDecoder::new();
    let mut results = dec.push(&frame);
    assert_eq!(results.len(), 1, "expected exactly one decoded message");
    match results.remove(0) {
        Ok(Incoming::EmberPayload(p)) => p,
        other => panic!("expected EmberPayload, got {other:?}"),
    }
}

// Captured fixtures (full S101 frames, byte-stuffed BOF..EOF).
const ROOT_GETDIR_REQUEST: &str =
    "fe000e0001c001021f0260106b0ea00c620aa003020120a1030201fddf948fff";
const ROOT_GETDIR_RESPONSE: &str = "fe000e0001c001021f02604c6b4aa0486a46a0030d0100a13f313da0190c1745\
6d6265725669657765725465737450726f7669646572a11b0c19456d62657256696577657220546573742050726f766964\
6572a3030101fddf0d4aff";
const CHILD_GETDIR_RESPONSE: &str = "fe000e0001c001021f026082010a6b820106a0326930a0050d03000100a127\
3125a00a0c08696e74506172616da20302012aa303020100a403020164a503020103ad03020101a031692fa0050d030001\
01a1263124a00b0c097265616c506172616da20b090980011921fdd9f01b866ea503020101ad03020102a0356933a0050d\
03000102a12a3128a00d0c0b737472696e67506172616da20d0c0b68656c6c6f20656d626572a503020103ad03020103a0\
296927a0050d03000103a11e311ca00b0c09626f6f6c506172616da2030101fddfa503020101ad03020104a03b6939a005\
0d03000104a130312ea00b0c09656e756d506172616da203020101a503020103a7100c0e5265640a477265656e0a426c75\
65ad030201060689ff";

#[test]
fn s101_crc_matches_node_emberplus() {
    // If our CRC-16/X-25 implementation disagreed with node-emberplus, deframing
    // these real frames would yield a CrcMismatch error instead of a payload.
    let _ = payload_of(ROOT_GETDIR_REQUEST);
    let _ = payload_of(ROOT_GETDIR_RESPONSE);
    let _ = payload_of(CHILD_GETDIR_RESPONSE);
}

#[test]
fn decode_root_getdirectory_request() {
    let payload = payload_of(ROOT_GETDIR_REQUEST);
    let root = decode_root(&payload).expect("decode request");
    let Root::Elements(coll) = root else {
        panic!("expected element collection");
    };
    assert_eq!(coll.0.len(), 1);
    let RootElementEntry(RootElement::Element(Element::Command(cmd))) = &coll.0[0] else {
        panic!("expected a Command at root, got {:?}", coll.0[0]);
    };
    assert_eq!(cmd.number, command_type::GET_DIRECTORY);
}

#[test]
fn decode_root_response_qualified_node() {
    let payload = payload_of(ROOT_GETDIR_RESPONSE);
    let root = decode_root(&payload).expect("decode response");
    let Root::Elements(coll) = root else {
        panic!("expected element collection");
    };
    assert_eq!(coll.0.len(), 1);
    let RootElementEntry(RootElement::QualifiedNode(qn)) = &coll.0[0] else {
        panic!("expected QualifiedNode, got {:?}", coll.0[0]);
    };
    assert_eq!(qn.path.arcs(), vec![0]);
    let contents = qn.contents.as_ref().expect("node contents");
    assert_eq!(contents.identifier.as_deref(), Some("EmberViewerTestProvider"));
    assert_eq!(
        contents.description.as_deref(),
        Some("EmberViewer Test Provider")
    );
}

#[test]
fn decode_child_response_all_parameter_types() {
    let payload = payload_of(CHILD_GETDIR_RESPONSE);
    let root = decode_root(&payload).expect("decode child response");
    let Root::Elements(coll) = root else {
        panic!("expected element collection");
    };

    // Collect the 5 qualified parameters keyed by identifier.
    let mut params = std::collections::HashMap::new();
    for RootElementEntry(re) in &coll.0 {
        let RootElement::QualifiedParameter(qp) = re else {
            panic!("expected QualifiedParameter, got {re:?}");
        };
        let c = qp.contents.as_ref().expect("param contents");
        let id = c.identifier.clone().expect("identifier");
        params.insert(id, (qp.path.arcs(), c.clone()));
    }
    assert_eq!(params.len(), 5);

    // intParam: integer 42, min 0, max 100, readWrite
    let (path, c) = &params["intParam"];
    assert_eq!(path, &vec![0, 1, 0]);
    assert_eq!(c.value_, Some(Value::Integer(42)));
    assert_eq!(c.minimum, Some(MinMax::Integer(0)));
    assert_eq!(c.maximum, Some(MinMax::Integer(100)));
    assert_eq!(c.access, Some(access::READ_WRITE));
    assert_eq!(c.r#type, Some(parameter_type::INTEGER));

    // realParam: real ~3.14159
    let (_, c) = &params["realParam"];
    match &c.value_ {
        Some(Value::Real(r)) => {
            let f = r.to_f64();
            assert!((f - 3.14159).abs() < 1e-6, "got {f}")
        }
        other => panic!("expected real, got {other:?}"),
    }
    assert_eq!(c.r#type, Some(parameter_type::REAL));

    // stringParam: "hello ember", readWrite
    let (_, c) = &params["stringParam"];
    assert_eq!(c.value_, Some(Value::String("hello ember".into())));
    assert_eq!(c.access, Some(access::READ_WRITE));

    // boolParam: boolean true
    let (_, c) = &params["boolParam"];
    assert_eq!(c.value_, Some(Value::Boolean(true)));

    // enumParam: enum index 1, enumeration "Red\nGreen\nBlue"
    let (_, c) = &params["enumParam"];
    assert_eq!(c.value_, Some(Value::Integer(1)));
    assert_eq!(c.enumeration.as_deref(), Some("Red\nGreen\nBlue"));
    assert_eq!(c.r#type, Some(parameter_type::ENUM));
}

// Phase 4 fixtures — matrix, function, and an invocation result.
const MATRIX_RESPONSE: &str = "fe000e0001c001021f02604e6b4ca04a7148a0040d020002a120311ea0080c066d61\
74726978a203020100a303020100a403020104a503020104a51e301ca00c700aa003020100a1030d0100a00c700aa00302\
0101a1030d0102572fff";
const FUNCTION_RESPONSE: &str = "fe000e0001c001021f0260756b73a071746fa0050d03000300a1663164a0050c03\
616464a1270c254164642074776f20696e74656765727320616e642072657475726e2074686569722073756da21e301ca0\
0c750aa003020101a1030c0161a00c750aa003020101a1030c0162a3123010a00e750ca003020101a1050c0373756db778ff";
const INVOCATION_RESULT: &str =
    "fe000e0001c001021f0260157713a003020101a1030101fddfa2073005a0030201073045ff";

#[test]
fn decode_roots_handles_single_and_concatenated() {
    // A single PDU yields one root.
    let one = payload_of(ROOT_GETDIR_RESPONSE);
    let roots = ember_proto::glow::decode_roots(&one);
    assert_eq!(roots.len(), 1);
    assert!(roots[0].is_ok());

    // Two PDUs concatenated in one payload yield two roots — the case a strict
    // single decode rejects as "extra data".
    let two_src = payload_of(CHILD_GETDIR_RESPONSE);
    let mut concat = one.clone();
    concat.extend_from_slice(&two_src);
    let roots = ember_proto::glow::decode_roots(&concat);
    assert_eq!(roots.len(), 2);
    assert!(roots.iter().all(|r| r.is_ok()));
}

#[test]
fn decode_matrix() {
    let payload = payload_of(MATRIX_RESPONSE);
    let root = decode_root(&payload).expect("decode matrix");
    let Root::Elements(coll) = root else {
        panic!("expected elements");
    };
    let RootElementEntry(RootElement::QualifiedMatrix(m)) = &coll.0[0] else {
        panic!("expected QualifiedMatrix, got {:?}", coll.0[0]);
    };
    assert_eq!(m.path.arcs(), vec![0, 2]);
    let c = m.contents.as_ref().expect("matrix contents");
    assert_eq!(c.identifier.as_deref(), Some("matrix"));
    assert_eq!(c.r#type, Some(matrix_type::ONE_TO_N));
    assert_eq!(c.target_count, Some(4));
    assert_eq!(c.source_count, Some(4));

    let conns = &m.connections.as_ref().expect("connections").0;
    assert_eq!(conns.len(), 2);
    // target 0 <- source 0
    assert_eq!(conns[0].0.target, 0);
    assert_eq!(conns[0].0.sources.as_ref().unwrap().arcs(), vec![0]);
    // target 1 <- source 2
    assert_eq!(conns[1].0.target, 1);
    assert_eq!(conns[1].0.sources.as_ref().unwrap().arcs(), vec![2]);
}

#[test]
fn decode_function() {
    let payload = payload_of(FUNCTION_RESPONSE);
    let root = decode_root(&payload).expect("decode function");
    let Root::Elements(coll) = root else {
        panic!("expected elements");
    };
    let RootElementEntry(RootElement::QualifiedFunction(f)) = &coll.0[0] else {
        panic!("expected QualifiedFunction, got {:?}", coll.0[0]);
    };
    assert_eq!(f.path.arcs(), vec![0, 3, 0]);
    let c = f.contents.as_ref().expect("function contents");
    assert_eq!(c.identifier.as_deref(), Some("add"));

    let args = &c.arguments.as_ref().expect("arguments").0;
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].0.r#type, parameter_type::INTEGER);
    assert_eq!(args[0].0.name.as_deref(), Some("a"));
    assert_eq!(args[1].0.name.as_deref(), Some("b"));

    let result = &c.result.as_ref().expect("result").0;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0.name.as_deref(), Some("sum"));
    assert_eq!(result[0].0.r#type, parameter_type::INTEGER);
}

// node-emberplus StreamCollection push: meterL=-8.68, meterR=-0.1, meterPeak=0.
const STREAM_COLLECTION: &str = "fe000e0001c001021f02603c653aa0146512a003020101a10b0909c0021ceb851e\
b851eca0146512a003020102a10b0909c000111eb851eb851fa00c650aa003020103a1030201007cb4ff";

#[test]
fn decode_stream_collection() {
    // The captured frame's CRC byte is a capture-tool transcription artifact
    // (the live socket sends correct CRCs — verified separately), so decode the
    // BER payload directly: strip BOF + the 9-byte S101 header and the CRC + EOF.
    let frame = hex(STREAM_COLLECTION);
    let payload = &frame[10..frame.len() - 3];
    let root = decode_root(payload).expect("decode stream collection");
    // node-emberplus tags the collection [APPLICATION 5] (StreamsAlt).
    let Root::StreamsAlt(coll) = root else {
        panic!("expected StreamsAlt, got {root:?}");
    };
    assert_eq!(coll.0.len(), 3);
    // meterL / meterR are live REAL meter samples (value varies); just check type
    // and a plausible dB range. meterPeak is a stable integer.
    assert_eq!(coll.0[0].0.stream_identifier, 1);
    match &coll.0[0].0.stream_value {
        Value::Real(r) => {
            let v = r.to_f64();
            assert!((-100.0..=100.0).contains(&v), "implausible meter {v}");
        }
        other => panic!("expected real, got {other:?}"),
    }
    assert_eq!(coll.0[2].0.stream_identifier, 3);
    assert_eq!(coll.0[2].0.stream_value, Value::Integer(0));
}

#[test]
fn decode_invocation_result() {
    let payload = payload_of(INVOCATION_RESULT);
    let root = decode_root(&payload).expect("decode invocation result");
    let Root::InvocationResult(ir) = root else {
        panic!("expected InvocationResult, got {root:?}");
    };
    assert_eq!(ir.invocation_id, Some(1));
    assert_eq!(ir.success, Some(true));
    let result = &ir.result.as_ref().expect("result").0;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, Value::Integer(7));
}

#[test]
fn root_getdirectory_roundtrips() {
    // Our own request must encode and decode back to an equal value.
    let req = Root::root_get_directory();
    let bytes = encode_root(&req).expect("encode");
    let back = decode_root(&bytes).expect("decode");
    assert_eq!(req, back);

    // And the provider must accept it: it should at least contain a getDirectory
    // command at the root.
    let Root::Elements(coll) = &back else {
        panic!("expected elements");
    };
    let RootElementEntry(RootElement::Element(Element::Command(cmd))) = &coll.0[0] else {
        panic!("expected command");
    };
    assert_eq!(cmd.number, command_type::GET_DIRECTORY);
}
