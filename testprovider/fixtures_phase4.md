# Ember+ Phase 4 wire fixtures (matrix + function)

Captured from a node-emberplus client <-> the test provider on 127.0.0.1:9000
(node-emberplus v3.0.8). All frames are full S101 frames, hex-encoded:

    FE <slot msg cmd ver flags dtd appLen> <BER payload> <CRC16> FF

S101 header for every response below is: `00 0e 00 01 c0 01 02` followed by
`1f 02` (appBytes: glow DTD descriptor) then the BER payload. The BER payload is
unstuffed (no 0xFD escapes occur in these particular frames). The trailing 2
bytes before 0xFF are the S101 CRC.

Relevant Ember+/Glow BER tags used below:
- `0x60` APPLICATION 0  = Root / RootElementCollection wrapper
- `0x6b` APPLICATION 11 = Matrix
- `0x71` APPLICATION 17 = MatrixContents
- `0x70` APPLICATION 16 = MatrixConnection
- `0x74` APPLICATION 20 = Function
- `0x73` APPLICATION 19 = FunctionContents
- `0x75` APPLICATION 21 = FunctionArgument (tuple of type + name)
- `0x77` APPLICATION 23 = InvocationResult
- `0x0d` UNIVERSAL 13   = RELATIVE OID (used for connection sources and qualified paths)
- `0x0c` UNIVERSAL 12   = UTF8String
- `0x02` UNIVERSAL 2    = INTEGER
- `0x01` UNIVERSAL 1    = BOOLEAN
- `0x31` SET, `0x30` SEQUENCE
- `0xaN` = CONTEXT[N] constructed

ParameterType encoding (CONTEXT[0] of a FunctionArgument): integer=1, real=2,
string=3, boolean=4, trigger=5, enum=6, octets=7.

---

## 1. Matrix getDirectory RESPONSE  (node "matrix", path `0.2`)

A 4x4 one-to-N linear matrix with two initial crosspoints
(target 0 <- source 0, target 1 <- source 2).

Full S101 frame (93 bytes):

    fe000e0001c001021f02604e6b4ca04a7148a0040d020002a120311ea0080c066d6174726978a203020100a303020100a403020104a503020104a51e301ca00c700aa003020100a1030d0100a00c700aa003020101a1030d0102572fff

### Decoded BER structure

    [APP 0  cons] 0x60  Root len=78
      [APP 11 cons] 0x6b  Matrix len=76
        [CTX 0  cons] 0xa0  number len=74
          [APP 17 cons] 0x71  MatrixContents len=72
            [CTX 0 cons] 0xa0  number          0x0d 0002        -> number = 2   (path component)
            [CTX 1 cons] 0xa1  contents (SET 0x31)
              [CTX 0 cons] 0xa0  identifier   0x0c "matrix"
              [CTX 2 cons] 0xa2  type         0x02 00          -> oneToN (0)
              [CTX 3 cons] 0xa3  mode         0x02 00          -> linear (0)
              [CTX 4 cons] 0xa4  targetCount  0x02 04          -> 4
              [CTX 5 cons] 0xa5  sourceCount  0x02 04          -> 4
            [CTX 5 cons] 0xa5  connections (SEQUENCE 0x30)
              [CTX 0 cons] 0xa0  connection
                [APP 16 cons] 0x70  MatrixConnection
                  [CTX 0 cons] 0xa0  target    0x02 00         -> target 0
                  [CTX 1 cons] 0xa1  sources   0x0d 00         -> RELATIVE-OID "0"  (source 0)
              [CTX 0 cons] 0xa0  connection
                [APP 16 cons] 0x70  MatrixConnection
                  [CTX 0 cons] 0xa0  target    0x02 01         -> target 1
                  [CTX 1 cons] 0xa1  sources   0x0d 02         -> RELATIVE-OID "2"  (source 2)

Notes:
- Matrix descriptor fields live in MatrixContents (APP 17) under Matrix.contents
  (CTX 1). `type` (CTX 2) and `mode` (CTX 3) are integers; targetCount (CTX 4)
  and sourceCount (CTX 5) are integers.
- Targets (Matrix CTX 3) and Sources (Matrix CTX 4) explicit lists are NOT
  emitted here because all 4 targets/sources are the default linear 0..N-1;
  node-emberplus only sends explicit targets/sources when they deviate. The
  client derives the 4 targets / 4 sources from targetCount / sourceCount.
- Connections (Matrix CTX 5) is a sequence of MatrixConnection (APP 16); each
  has target (CTX 0, INTEGER) and sources (CTX 1, RELATIVE-OID whose dotted path
  components are the source numbers).
- No `labels` descriptor is emitted (intentionally omitted in the provider: the
  node-emberplus server times out encoding a matrix that has both labels and
  matrix children).

### Decoded matrix (client.toJSON)

```json
{
  "number": 2,
  "path": "0.2",
  "type": "oneToN",
  "mode": "linear",
  "connections": {
    "0": { "target": 0, "sources": [0] },
    "1": { "target": 1, "sources": [2] }
  },
  "identifier": "matrix",
  "targetCount": 4,
  "sourceCount": 4,
  "labels": []
}
```

To set a crosspoint the client sends a Matrix (APP 11) with a connections
(CTX 5) child holding a MatrixConnection (APP 16) carrying target (CTX 0),
sources (CTX 1 RELATIVE-OID) and operation (CTX 2 INTEGER; 0=absolute,
1=connect, 2=disconnect).

---

## 2. Function getDirectory RESPONSE  (node "add", path `0.3.0`)

`add(a: integer, b: integer) -> sum: integer`.

Full S101 frame (132 bytes):

    fe000e0001c001021f0260756b73a071746fa0050d03000300a1663164a0050c03616464a1270c254164642074776f20696e74656765727320616e642072657475726e2074686569722073756da21e301ca00c750aa003020101a1030c0161a00c750aa003020101a1030c0162a3123010a00e750ca003020101a1050c0373756db778ff

### Decoded BER structure

    [APP 0  cons] 0x60  Root len=117
      [APP 11 cons] 0x6b  (QualifiedNode wrapper) len=115
        [CTX 0  cons] 0xa0  len=113
          [APP 20 cons] 0x74  Function len=111
            [CTX 0 cons] 0xa0  number        0x0d 000300       -> RELATIVE-OID "0.3.0" (qualified path)
            [CTX 1 cons] 0xa1  contents (SET 0x31)
              [CTX 0 cons] 0xa0  identifier  0x0c "add"
              [CTX 1 cons] 0xa1  description 0x0c "Add two integers and return their sum"
              [CTX 2 cons] 0xa2  arguments (SEQUENCE 0x30)
                [CTX 0 cons] 0xa0  arg
                  [APP 21 cons] 0x75  FunctionArgument
                    [CTX 0 cons] 0xa0  type  0x02 01           -> integer (ParameterType 1)
                    [CTX 1 cons] 0xa1  name  0x0c "a"
                [CTX 0 cons] 0xa0  arg
                  [APP 21 cons] 0x75  FunctionArgument
                    [CTX 0 cons] 0xa0  type  0x02 01           -> integer (1)
                    [CTX 1 cons] 0xa1  name  0x0c "b"
              [CTX 3 cons] 0xa3  result (SEQUENCE 0x30)
                [CTX 0 cons] 0xa0  res
                  [APP 21 cons] 0x75  FunctionArgument
                    [CTX 0 cons] 0xa0  type  0x02 01           -> integer (1)
                    [CTX 1 cons] 0xa1  name  0x0c "sum"

Notes:
- The provider returns the function as a *qualified* element: number (CTX 0) is
  a RELATIVE-OID `0.3.0` (bytes `00 03 00`) rather than a single integer, hence
  the outer wrapper is APP 11 / Function APP 20 carrying a path.
- FunctionContents lives under Function.contents (CTX 1, SET 0x31):
  identifier (CTX 0), description (CTX 1), arguments (CTX 2), result (CTX 3).
- Both `arguments` and `result` are SEQUENCEs of CONTEXT[0]-wrapped
  FunctionArgument (APP 21). Each FunctionArgument is a tuple of
  type (CTX 0 INTEGER = ParameterType) and name (CTX 1 UTF8String). No value is
  carried in the descriptor (value only appears in Invocation / InvocationResult).

### Decoded function (client.toJSON, minimal qualified stub)

```json
{
  "nodeType": "QualifiedFunction",
  "number": 0,
  "path": "0.3.0"
}
```

(The full argument/result typing is in the wire frame above; the client's
toJSON of the qualified leaf is minimal, so rely on the BER breakdown.)

---

## 3. InvocationResult RESPONSE  (invoke add(3, 4) -> 7)

Full S101 frame (37 bytes):

    fe000e0001c001021f0260157713a003020101a1030101fddfa2073005a0030201073045ff

### Decoded BER structure

    [APP 0  cons] 0x60  Root len=21
      [APP 23 cons] 0x77  InvocationResult len=19
        [CTX 0 cons] 0xa0  invocationId 0x02 01            -> 1
        [CTX 1 cons] 0xa1  success      0x01 ff            -> BOOLEAN true
        [CTX 2 cons] 0xa2  result (SEQUENCE 0x30)
          [CTX 0 cons] 0xa0  0x02 07                       -> INTEGER 7   (sum = 3 + 4)

Notes:
- InvocationResult is APP 23: invocationId (CTX 0 INTEGER), success
  (CTX 1 BOOLEAN), result (CTX 2 SEQUENCE of CONTEXT[0]-wrapped typed values).
- The result values are bare typed primitives (here INTEGER 7) wrapped in
  CONTEXT[0]; the type tag is the BER universal type (0x02 INTEGER).
- The matching Invocation request (client -> provider) wraps the function path in
  a Command (APP 2) with an Invocation carrying the argument values; the provider
  echoes the same invocationId.

### Decoded invocation result (client)

```json
{
  "invocationId": 1,
  "success": true,
  "result": [ { "type": "integer", "value": 7 } ]
}
```
