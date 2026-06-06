// Connects a node-emberplus client to the local provider, issues GetDirectory
// on the root and then on a child node ("audio"/"parameters"), and captures the
// exact wire bytes sent and received at the TCP layer (full S101/Ember+ frames).
// Dumps them as labeled hex for use as Rust test fixtures and writes fixtures.md.
const net = require('net');
const fs = require('fs');
const path = require('path');
const { EmberClient } = require('node-emberplus');

const HOST = '127.0.0.1';
const PORT = 9000;

const sent = [];
const recv = [];

// Monkey-patch net.Socket at the prototype level so we record every chunk
// written/received on the client's underlying TCP connection (raw S101 frames).
const origWrite = net.Socket.prototype.write;
net.Socket.prototype.write = function (chunk, ...rest) {
  if (Buffer.isBuffer(chunk)) sent.push(Buffer.from(chunk));
  else if (typeof chunk === 'string') sent.push(Buffer.from(chunk));
  return origWrite.call(this, chunk, ...rest);
};
const origConnect = net.Socket.prototype.connect;
net.Socket.prototype.connect = function (...args) {
  this.on('data', (d) => recv.push(Buffer.from(d)));
  return origConnect.apply(this, args);
};

const hex = (buf) => buf.toString('hex');

(async () => {
  const client = new EmberClient({ host: HOST, port: PORT });
  await client.connectAsync();

  // --- 1. Root GetDirectory ---
  let s0 = sent.length, r0 = recv.length;
  await client.getDirectoryAsync();
  await new Promise((r) => setTimeout(r, 300));
  const rootSent = sent.slice(s0);
  const rootRecv = recv.slice(r0);

  // Root GetDirectory only returns the top node; expand it to discover its
  // children (identity, audio/parameters).
  const rootChildren = client.root.getChildren() || [];
  const top = rootChildren[0];
  await client.getDirectoryAsync(top);
  await new Promise((r) => setTimeout(r, 300));
  const topChildren = top.getChildren() || [];
  let childNode =
    topChildren.find((c) => {
      const ident = c.contents && c.contents.identifier;
      return ident === 'audio' || ident === 'parameters';
    }) || topChildren[topChildren.length - 1];

  const childPath = childNode.getPath();
  const childIdent = childNode.contents && childNode.contents.identifier;

  // --- 2. Child GetDirectory ---
  let s1 = sent.length, r1 = recv.length;
  await client.getDirectoryAsync(childNode);
  await new Promise((r) => setTimeout(r, 300));
  const childSent = sent.slice(s1);
  const childRecv = recv.slice(r1);

  const treeJson = client.root.toJSON();

  // ---- console output ----
  console.log('=== ROOT GetDirectory REQUEST (client -> provider) ===');
  rootSent.forEach((b, i) => console.log(`req[${i}] (${b.length}B): ${hex(b)}`));
  console.log('=== ROOT GetDirectory RESPONSE (provider -> client) ===');
  rootRecv.forEach((b, i) => console.log(`resp[${i}] (${b.length}B): ${hex(b)}`));
  console.log(`=== CHILD GetDirectory REQUEST (node "${childIdent}" path ${childPath}) ===`);
  childSent.forEach((b, i) => console.log(`req[${i}] (${b.length}B): ${hex(b)}`));
  console.log('=== CHILD GetDirectory RESPONSE ===');
  childRecv.forEach((b, i) => console.log(`resp[${i}] (${b.length}B): ${hex(b)}`));
  console.log('=== DECODED TREE (client.root.toJSON) ===');
  console.log(JSON.stringify(treeJson, null, 2));

  // ---- fixtures.md ----
  const cat = (frames) => frames.map(hex).join('');
  let md = '';
  md += '# Ember+ wire fixtures\n\n';
  md += 'Captured from node-emberplus client <-> provider on 127.0.0.1:9000.\n';
  md += 'All values are full S101 frames (0xFE ... 0xFF byte-stuffed, BER payload), hex-encoded.\n\n';

  md += '## 1. Root GetDirectory REQUEST (client -> provider)\n\n';
  rootSent.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
  md += `Concatenated: \`${cat(rootSent)}\`\n\n`;

  md += '## 2. Root GetDirectory RESPONSE (provider -> client)\n\n';
  rootRecv.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
  md += `Concatenated: \`${cat(rootRecv)}\`\n\n`;

  md += `## 3. Child GetDirectory REQUEST — node "${childIdent}" (path ${childPath})\n\n`;
  childSent.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
  md += `Concatenated: \`${cat(childSent)}\`\n\n`;

  md += '## 4. Child GetDirectory RESPONSE (provider -> client)\n\n';
  childRecv.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
  md += `Concatenated: \`${cat(childRecv)}\`\n\n`;

  md += '## 5. Decoded tree (for Rust assertions)\n\n';
  md += '```json\n' + JSON.stringify(treeJson, null, 2) + '\n```\n';

  fs.writeFileSync(path.join(__dirname, 'fixtures.md'), md);
  console.log('\nWrote fixtures.md');

  await client.disconnectAsync();
  process.exit(0);
})().catch((e) => {
  console.error(e.stack);
  process.exit(1);
});
