// Captures wire fixtures for the matrix (0.2) and function (0.3.0) nodes.
// Records raw S101 frames at the TCP layer, navigates to the matrix and the
// function, captures their getDirectory RESPONSE frames and an InvocationResult,
// and writes fixtures_phase4.md with hex + decoded JSON.
const net = require('net');
const fs = require('fs');
const path = require('path');
const { EmberClient } = require('node-emberplus');

const HOST = '127.0.0.1';
const PORT = 9000;

const recv = [];
const origConnect = net.Socket.prototype.connect;
net.Socket.prototype.connect = function (...args) {
  this.on('data', (d) => recv.push(Buffer.from(d)));
  return origConnect.apply(this, args);
};
const hex = (buf) => buf.toString('hex');
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const client = new EmberClient({ host: HOST, port: PORT });
  await client.connectAsync();
  await client.getDirectoryAsync();
  await wait(200);

  // Expand root child to discover identity/parameters/matrix/functions.
  const top = client.root.getChildren()[0];
  await client.getDirectoryAsync(top);
  await wait(200);
  const topChildren = top.getChildren();
  const matrixNode = topChildren.find((c) => c.contents && c.contents.identifier === 'matrix');
  const funcsNode = topChildren.find((c) => c.contents && c.contents.identifier === 'functions');

  // --- Matrix getDirectory RESPONSE ---
  let r0 = recv.length;
  await client.getDirectoryAsync(matrixNode);
  await wait(300);
  const matrixRecv = recv.slice(r0);
  const matrixPath = matrixNode.getPath();

  // --- Functions node getDirectory (to reach the function child) ---
  await client.getDirectoryAsync(funcsNode);
  await wait(200);
  const funcChild = funcsNode.getChildren().find((c) => c.contents && c.contents.identifier === 'add');

  // --- Function getDirectory RESPONSE (returns its FunctionContents) ---
  r0 = recv.length;
  await client.getDirectoryAsync(funcChild);
  await wait(300);
  const funcRecv = recv.slice(r0);
  const funcPath = funcChild.getPath();

  // --- Invoke the function add(3, 4) -> 7 ---
  let invRecv = [];
  let invResult = null;
  try {
    r0 = recv.length;
    const { FunctionArgument, ParameterType } = require('node-emberplus').EmberLib;
    const res = await client.invokeFunctionAsync(funcChild, [
      new FunctionArgument(ParameterType.integer, 3),
      new FunctionArgument(ParameterType.integer, 4),
    ]);
    await wait(300);
    invRecv = recv.slice(r0);
    invResult = res && res.toJSON ? res.toJSON() : res;
  } catch (e) {
    invResult = { error: e.message };
  }

  const matrixJson = matrixNode.toJSON();
  const funcJson = funcChild.toJSON();

  const cat = (frames) => frames.map(hex).join('');

  // Console
  console.log(`MATRIX path ${matrixPath}`);
  matrixRecv.forEach((b, i) => console.log(`matrix.resp[${i}] (${b.length}B): ${hex(b)}`));
  console.log(JSON.stringify(matrixJson, null, 2));
  console.log(`FUNCTION path ${funcPath}`);
  funcRecv.forEach((b, i) => console.log(`func.resp[${i}] (${b.length}B): ${hex(b)}`));
  console.log(JSON.stringify(funcJson, null, 2));
  console.log('INVOCATION RESULT', JSON.stringify(invResult, null, 2));
  invRecv.forEach((b, i) => console.log(`inv.resp[${i}] (${b.length}B): ${hex(b)}`));

  // Markdown
  let md = '# Ember+ Phase 4 wire fixtures (matrix + function)\n\n';
  md += 'Captured from node-emberplus client <-> provider on 127.0.0.1:9000.\n';
  md += 'All frames are full S101 frames (0xFE ... 0xFF, byte-stuffed, BER payload), hex-encoded.\n\n';

  md += `## Matrix getDirectory RESPONSE (node "matrix", path ${matrixPath})\n\n`;
  matrixRecv.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
  md += `Concatenated: \`${cat(matrixRecv)}\`\n\n`;
  md += '### Decoded matrix (client.toJSON)\n\n```json\n' + JSON.stringify(matrixJson, null, 2) + '\n```\n\n';

  md += `## Function getDirectory RESPONSE (node "add", path ${funcPath})\n\n`;
  funcRecv.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
  md += `Concatenated: \`${cat(funcRecv)}\`\n\n`;
  md += '### Decoded function (client.toJSON)\n\n```json\n' + JSON.stringify(funcJson, null, 2) + '\n```\n\n';

  md += '## InvocationResult RESPONSE (add(3, 4))\n\n';
  invRecv.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
  if (invRecv.length) md += `Concatenated: \`${cat(invRecv)}\`\n\n`;
  md += '### Decoded invocation result\n\n```json\n' + JSON.stringify(invResult, null, 2) + '\n```\n';

  fs.writeFileSync(path.join(__dirname, 'fixtures_phase4.md'), md);
  console.log('\nWrote fixtures_phase4.md');
  await client.disconnectAsync();
  process.exit(0);
})().catch((e) => { console.error(e.stack); process.exit(1); });
