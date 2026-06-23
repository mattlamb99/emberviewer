// Ember+ test provider for integration-testing the Rust emberviewer client.
// Starts a provider on 127.0.0.1:9000 with a small sample tree containing
// parameters of several types (integer, real, string, boolean, enum),
// including at least one writable parameter.
//
// Usage: node server.js
const { EmberServer, EmberServerEvent, EmberLib } = require('node-emberplus');
const { ParameterType, FunctionArgument, TreeNode, StreamCollection, StreamEntry } = EmberLib;

const HOST = '0.0.0.0';
//const HOST = '127.0.0.1';
const PORT = 9000;

// Build a oneToN matrix with a connection entry pre-created for EVERY target
// (node-emberplus's connect handler dereferences matrix.connections[target]
// before creating it, so undefined targets crash it and reject routes).
// `initial` maps target index -> array of initial source indices.
function makeMatrix(identifier, targetCount, sourceCount, initial = {}) {
    const connections = {};
    for (let t = 0; t < targetCount; t++) {
        connections[t] = { target: t, sources: initial[t] || [] };
    }
    return { identifier, type: 'oneToN', mode: 'linear', targetCount, sourceCount, connections };
}

// Sample tree:
// 0                       "EmberViewer Test Provider" (root node)
// 0.0                     "identity"        (node)
// 0.0.0   product         string  (ro)
// 0.0.1   version         string  (ro)
// 0.0.2   company         string  (rw)   <- writable
// 0.1                     "parameters"      (node)
// 0.1.0   intParam        integer (rw, min 0 max 100)  <- writable
// 0.1.1   realParam       real    (ro)
// 0.1.2   stringParam     string  (rw)   <- writable
// 0.1.3   boolParam       boolean (ro)
// 0.1.4   enumParam       enum    (rw)   <- writable
const jsonTree = [
    {
        // path "0"
        identifier: 'EmberViewerTestProvider',
        description: 'EmberViewer Test Provider',
        children: [
            {
                // path "0.0"
                identifier: 'identity',
                children: [
                    { identifier: 'product', value: 'EmberViewer Test Provider', access: 'read' },
                    { identifier: 'version', value: '1.0.0', access: 'read' },
                    { identifier: 'company', value: 'L2', access: 'readWrite' },
                ],
            },
            {
                // path "0.1"
                identifier: 'parameters',
                children: [
                    {
                        identifier: 'intParam',
                        type: 'integer',
                        value: 42,
                        minimum: 0,
                        maximum: 100,
                        access: 'readWrite',
                    },
                    {
                        identifier: 'realParam',
                        type: 'real',
                        value: 3.14159,
                        access: 'read',
                    },
                    {
                        identifier: 'stringParam',
                        type: 'string',
                        value: 'hello ember',
                        access: 'readWrite',
                    },
                    {
                        identifier: 'sdp',
                        type: 'string',
                        value:
                            'v=0\r\n' +
                            'o=- 1234567890 1234567890 IN IP4 192.168.1.10\r\n' +
                            's=Stream 1\r\n' +
                            'c=IN IP4 239.0.0.1/32\r\n' +
                            't=0 0\r\n' +
                            'm=video 5004 RTP/AVP 96\r\n' +
                            'a=rtpmap:96 raw/90000\r\n' +
                            'a=fmtp:96 sampling=YCbCr-4:2:2; width=1920; height=1080; ' +
                            'exactframerate=50; depth=10; colorimetry=BT709; PM=2110GPM; ' +
                            'TP=2110TPN; SSN=ST2110-20:2017\r\n',
                        access: 'read',
                    },
                    {
                        identifier: 'sdpWritable',
                        type: 'string',
                        value: 'v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=edit me\r\n',
                        access: 'readWrite',
                    },
                    {
                        identifier: 'boolParam',
                        type: 'boolean',
                        value: true,
                        access: 'read',
                    },
                    {
                        identifier: 'enumParam',
                        type: 'enum',
                        value: 1,
                        enumeration: 'Red\nGreen\nBlue',
                        access: 'readWrite',
                    },
                ],
            },
            {
                // path "0.2"  -- a node holding two large routing matrices
                // (40 targets x 70 sources each) for testing bigger grids.
                identifier: 'matrices',
                children: [
                    makeMatrix('routerA', 40, 70, { 0: [0], 1: [2], 5: [10], 39: [69] }),
                    makeMatrix('routerB', 40, 70, { 3: [7], 20: [35] }),
                ],
            },
            {
                // path "0.4"  -- "interop" node exercising client features:
                //   enumMap, tilde-hidden enumeration, sliders (int/real),
                //   isOnline (offline node + param), and streamed meters.
                // Explicit number:4 keeps it at 0.4 even though it appears
                // before functions (0.3) in this array.
                number: 4,
                identifier: 'interop',
                children: [
                    {
                        // 0.4.0 -- integer parameter with a SPARSE enumMap.
                        // enumMap maps display name -> integer value with
                        // non-contiguous keys (0, 10, 20). Parser expects an
                        // array of {key, value}. Encoded as [15] enumMap ->
                        // StringIntegerCollection [APP 8] of StringIntegerPair
                        // [APP 7] (entryString [0] + entryInteger [1]).
                        identifier: 'enumMapParam',
                        type: 'integer',
                        value: 10,
                        access: 'readWrite',
                        enumMap: [
                            { key: 'Off', value: 0 },
                            { key: 'Low', value: 10 },
                            { key: 'High', value: 20 },
                        ],
                    },
                    {
                        // 0.4.1 -- enumeration string with a ~-prefixed entry
                        // to hide index 1 ("Reserved"). Indices 0,1,2 present,
                        // index 1 hidden. Encoded as [7] enumeration string.
                        identifier: 'tildeEnumParam',
                        type: 'enum',
                        value: 0,
                        access: 'readWrite',
                        enumeration: 'Red\n~Reserved\nBlue',
                    },
                    {
                        // 0.4.2 -- integer "gain" slider with min/max, a format
                        // string and a factor. minimum [3], maximum [4],
                        // format [6], factor [8].
                        identifier: 'gainSlider',
                        type: 'integer',
                        value: 0,
                        minimum: -60,
                        maximum: 12,
                        format: '%d dB',
                        factor: 1,
                        access: 'readWrite',
                    },
                    {
                        // 0.4.3 -- real "level" slider, min 0.0 max 1.0.
                        identifier: 'levelSlider',
                        type: 'real',
                        value: 0.5,
                        minimum: 0.0,
                        maximum: 1.0,
                        access: 'readWrite',
                    },
                    {
                        // 0.4.4 -- offline NODE (isOnline=false). NodeContents
                        // encodes isOnline at [3]. Has one child param so the
                        // client can see contents under an offline node.
                        identifier: 'offlineNode',
                        isOnline: false,
                        children: [
                            {
                                identifier: 'childParam',
                                type: 'string',
                                value: 'under offline node',
                                access: 'read',
                            },
                        ],
                    },
                    {
                        // 0.4.5 -- offline PARAMETER (isOnline=false).
                        // ParameterContents encodes isOnline at [9].
                        identifier: 'offlineParam',
                        type: 'integer',
                        value: 7,
                        isOnline: false,
                        access: 'read',
                    },
                    {
                        // 0.4.6 -- "meters" node holding streamed parameters.
                        // Each meter parameter carries a streamIdentifier [14];
                        // its live value is delivered out-of-band via a
                        // StreamCollection (see pushStreams() below) rather than
                        // the parameter's own [2] value.
                        identifier: 'meters',
                        children: [
                            {
                                // 0.4.6.0 -- real meter, streamIdentifier 1
                                identifier: 'meterL',
                                type: 'real',
                                value: 0.0,
                                minimum: -60.0,
                                maximum: 0.0,
                                access: 'read',
                                streamIdentifier: 1,
                            },
                            {
                                // 0.4.6.1 -- real meter, streamIdentifier 2
                                identifier: 'meterR',
                                type: 'real',
                                value: 0.0,
                                minimum: -60.0,
                                maximum: 0.0,
                                access: 'read',
                                streamIdentifier: 2,
                            },
                            {
                                // 0.4.6.2 -- integer meter, streamIdentifier 3
                                identifier: 'meterPeak',
                                type: 'integer',
                                value: 0,
                                minimum: -60,
                                maximum: 0,
                                access: 'read',
                                streamIdentifier: 3,
                            },
                        ],
                    },
                ],
            },
            {
                // path "0.3"  -- a node containing a real, invocable Function
                number: 3,
                identifier: 'functions',
                children: [
                    {
                        // path "0.3.0"  -- add(a, b) -> sum
                        identifier: 'add',
                        description: 'Add two integers and return their sum',
                        // Deterministic implementation: returns a + b as an integer.
                        func: (args) => {
                            const a = Number(args[0].value);
                            const b = Number(args[1].value);
                            return [new FunctionArgument(ParameterType.integer, a + b)];
                        },
                        // Typed arguments (ParameterType.integer === 1)
                        arguments: [
                            { type: ParameterType.integer, name: 'a' },
                            { type: ParameterType.integer, name: 'b' },
                        ],
                        // Typed result (one integer)
                        result: [
                            { type: ParameterType.integer, name: 'sum' },
                        ],
                    },
                ],
            },
        ],
    },
];

const root = EmberServer.createTreeFromJSON(jsonTree);
const server = new EmberServer({ host: HOST, port: PORT, tree: root });

server.on(EmberServerEvent.ERROR, (e) => {
    console.log('Server Error', e && e.stack ? e.stack : e);
});
server.on(EmberServerEvent.CLIENT_ERROR, (info) => {
    console.log('Client Error', info);
});
server.on(EmberServerEvent.CONNECTION, (info) => {
    console.log('New connection', info);
});
server.on(EmberServerEvent.VALUE_CHANGE, (node) => {
    try {
        console.log('Value changed:', node.getPath(), '=', node.contents && node.contents.value);
    } catch (e) {
        console.log('Value changed');
    }
});

// ---------------------------------------------------------------------------
// Stream pushing.
//
// node-emberplus has NO public "broadcast a StreamCollection" API, but a root
// TreeNode encodes its attached StreamCollection (TreeNode.setStreams +
// TreeNode.encode), and each connected client socket exposes queueMessage(node)
// which BER-encodes and frames a TreeNode over S101. So we build a bare root
// TreeNode, attach a StreamCollection of StreamEntry (streamIdentifier ->
// streamValue), and queue it to every connected client a few times a second.
//
// The meter parameters (0.4.6.x) carry streamIdentifier 1/2/3; their live
// values arrive here out-of-band rather than via the parameters' own values.
//
// NOTE: in node-emberplus, StreamCollection.BERID and StreamEntry.BERID are
// BOTH ber.APPLICATION(5) (the library does not use APP 6 for the collection).
// Each StreamEntry encodes streamIdentifier at CONTEXT(0) and streamValue at
// CONTEXT(1).
let streamPhase = 0;
function buildStreamCollection() {
    const sc = new StreamCollection();
    streamPhase += 0.3;
    // meterL (id 1) and meterR (id 2): real dBFS-ish values that sweep.
    const l = Math.round((-30 + 30 * Math.abs(Math.sin(streamPhase))) * 100) / 100;
    const r = Math.round((-30 + 30 * Math.abs(Math.sin(streamPhase + 0.7))) * 100) / 100;
    // meterPeak (id 3): integer peak hold.
    const peak = Math.round(Math.max(l, r));
    sc.addEntry(new StreamEntry(1, l));
    sc.addEntry(new StreamEntry(2, r));
    sc.addEntry(new StreamEntry(3, peak));
    return sc;
}

function pushStreams() {
    let clients;
    try {
        clients = server.clients; // Set<S101Socket> (not a documented API)
    } catch (e) {
        return;
    }
    if (clients == null || clients.size === 0) {
        return;
    }
    const sc = buildStreamCollection();
    const root = new TreeNode();
    root.setStreams(sc);
    for (const client of clients) {
        try {
            if (client.isConnected && client.isConnected()) {
                client.queueMessage(root);
            }
        } catch (e) {
            console.log('stream push error', e && e.message);
        }
    }
}

// ---------------------------------------------------------------------------
// isOnline toggling.
//
// Flip the offline node (0.4.4) and offline parameter (0.4.5) between
// online/offline every ~10s so the client can observe state changes, then
// push the updated subtree to subscribers via the server's setValue path is
// not applicable (no value change); instead we re-send the element. We use the
// server's createResponse via setValue-style update by toggling isOnline on
// the contents and queueing a getDirectory-style response to all clients.
let onlineState = false;
function toggleOnline() {
    onlineState = !onlineState;
    const targets = ['0.4.4', '0.4.5'];
    for (const p of targets) {
        const el = server.tree.getElementByPath(p);
        if (el == null || el.contents == null) {
            continue;
        }
        el.contents.isOnline = onlineState;
        // Re-encode just this element (with its updated contents) to all
        // clients so they observe the online/offline change. getDuplicate()
        // copies the element's contents onto a minimal node; getTreeBranch
        // wraps it back up to a root so it encodes as a valid response.
        try {
            const dup = el.getDuplicate();
            // Wrap dup (the element + its contents) up through its parent to a
            // root element so it encodes as a proper qualified response.
            const parent = el.getParent ? el.getParent() : null;
            const resp = parent != null ? parent.getTreeBranch(dup) : dup;
            for (const client of server.clients) {
                if (client.isConnected && client.isConnected()) {
                    client.queueMessage(resp);
                }
            }
        } catch (e) {
            // best effort; ignore if the helper isn't available.
        }
    }
    console.log('isOnline toggled ->', onlineState, 'for', targets.join(', '));
}

server
    .listen()
    .then(() => {
        console.log(`Ember+ provider listening on ${HOST}:${PORT}`);
        // Push stream meter frames ~5x/second.
        setInterval(pushStreams, 200);
        // Toggle the offline node/param every 10s.
        setInterval(toggleOnline, 10000);
    })
    .catch((e) => {
        console.log(e.stack);
        process.exit(1);
    });
