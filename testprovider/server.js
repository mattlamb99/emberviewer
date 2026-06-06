// Ember+ test provider for integration-testing the Rust emberviewer client.
// Starts a provider on 127.0.0.1:9000 with a small sample tree containing
// parameters of several types (integer, real, string, boolean, enum),
// including at least one writable parameter.
//
// Usage: node server.js
const { EmberServer, EmberServerEvent, EmberLib } = require('node-emberplus');
const { ParameterType, FunctionArgument } = EmberLib;

const HOST = '0.0.0.0';
//const HOST = '127.0.0.1';
const PORT = 9000;

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
                // path "0.2"  -- a real one-to-N routing matrix (4 targets x 4 sources).
                // Presence of `targetCount` triggers the matrix branch in the JSON parser.
                identifier: 'matrix',
                type: 'oneToN',
                mode: 'linear',
                targetCount: 4,
                sourceCount: 4,
                // Initial crosspoints: target 0 <- source 0, target 1 <- source 2.
                connections: {
                    0: { target: 0, sources: [0] },
                    1: { target: 1, sources: [2] },
                },
                // NOTE: a `labels` descriptor (basePath -> child label node) is
                // intentionally omitted: node-emberplus's server fails to encode a
                // matrix getDirectory response when labels + matrix children are both
                // present (the request times out). The matrix still exposes its
                // targets, sources and connections, which are the important parts here.
            },
            {
                // path "0.3"  -- a node containing a real, invocable Function
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

server
    .listen()
    .then(() => {
        console.log(`Ember+ provider listening on ${HOST}:${PORT}`);
    })
    .catch((e) => {
        console.log(e.stack);
        process.exit(1);
    });
