// Ember+ test provider for integration-testing the Rust emberviewer client.
// Starts a provider on 127.0.0.1:9000 with a small sample tree containing
// parameters of several types (integer, real, string, boolean, enum),
// including at least one writable parameter.
//
// Usage: node server.js
const { EmberServer, EmberServerEvent, EmberLib } = require('node-emberplus');
const { ParameterType } = EmberLib;

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
