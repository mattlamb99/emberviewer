// TCP MITM proxy: listens on 127.0.0.1:9001, forwards to the real provider on
// 127.0.0.1:9000, and logs every byte in each direction. A node-emberplus
// client connects to :9001, issues a root GetDirectory, and we capture the
// exact wire bytes (S101 framed Ember+) sent C->P and P->C.
const net = require('net');
const { EmberClient } = require('node-emberplus');

const PROXY_PORT = 9001;
const PROVIDER_HOST = '127.0.0.1';
const PROVIDER_PORT = 9000;

const c2p = []; // client -> provider chunks
const p2c = []; // provider -> client chunks

const proxy = net.createServer((clientSock) => {
    const upstream = net.connect(PROVIDER_PORT, PROVIDER_HOST);
    clientSock.on('data', (d) => {
        c2p.push(Buffer.from(d));
        upstream.write(d);
    });
    upstream.on('data', (d) => {
        p2c.push(Buffer.from(d));
        clientSock.write(d);
    });
    clientSock.on('close', () => upstream.end());
    upstream.on('close', () => clientSock.end());
    clientSock.on('error', () => {});
    upstream.on('error', () => {});
});

function hex(buf) {
    return buf.toString('hex');
}

proxy.listen(PROXY_PORT, '127.0.0.1', async () => {
    const client = new EmberClient({ host: '127.0.0.1', port: PROXY_PORT });
    await client.connectAsync();

    const c2pBefore = c2p.length;
    const p2cBefore = p2c.length;

    await client.getDirectoryAsync();
    await new Promise((r) => setTimeout(r, 400));

    const sentFrames = c2p.slice(c2pBefore);
    const recvFrames = p2c.slice(p2cBefore);

    console.log('=== CLIENT -> PROVIDER (root GetDirectory request) ===');
    sentFrames.forEach((b, i) => console.log(`c2p[${i}] (${b.length} bytes): ${hex(b)}`));

    console.log('=== PROVIDER -> CLIENT (root directory response) ===');
    recvFrames.forEach((b, i) => console.log(`p2c[${i}] (${b.length} bytes): ${hex(b)}`));

    await client.disconnectAsync();
    proxy.close();
    process.exit(0);
});
