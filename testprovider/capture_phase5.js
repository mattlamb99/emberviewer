// Captures wire fixtures for the new "interop" node (0.4) and its features:
//   - enumMap parameter (0.4.0)            -> getDirectory RESPONSE
//   - gain slider parameter (0.4.2)        -> getDirectory RESPONSE (min/max/format/factor)
//   - offline node (0.4.4) + param (0.4.5) -> getDirectory RESPONSE (isOnline)
//   - streamed meters (0.4.6.x)            -> a StreamCollection frame pushed by the provider
//
// Records raw S101 frames at the TCP layer (net.Socket data hook), navigates the
// tree with a node-emberplus client, and writes fixtures_phase5.md with full
// S101 frame hex + a decoded BER tag breakdown for each fixture.
const net = require('net');
const fs = require('fs');
const path = require('path');
const { EmberClient, EmberLib } = require('node-emberplus');
// Use node-emberplus's own BER reader to decode REAL values (its base-2 REAL
// encoding leaves the exponent-length bits zero, so a naive X.690 reader can't
// reverse it - but its matching ExtendedReader.readReal can).
const { ExtendedReader, EMBER_REAL } = require('node-emberplus/lib/ber');

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

// ---------------------------------------------------------------------------
// Minimal S101 + BER tag walker for the decoded breakdown.
// Unframes one S101 packet (0xFE ... 0xFF, byte-stuffed with 0xFD escape,
// 0xFD/0xFE/0xFF -> 0xFD <b XOR 0x20>), drops the 5-byte S101 header + CRC/EOF,
// and pretty-prints the BER TLV tree with class/tag annotations.
function s101Unframe(buf) {
  // find 0xFE ... 0xFF
  const start = buf.indexOf(0xfe);
  if (start < 0) return null;
  const end = buf.indexOf(0xff, start + 1);
  if (end < 0) return null;
  const body = [];
  for (let i = start + 1; i < end; i++) {
    let b = buf[i];
    if (b === 0xfd) {
      i++;
      b = buf[i] ^ 0x20;
    }
    body.push(b);
  }
  return Buffer.from(body);
}

// S101 header is: slot, message, command, version, flags, dtd, appBytes, [app...].
// For Ember the payload begins after the header. We locate the payload by
// scanning for the first BER universal/application sequence start (0x60 = APP 0
// constructed, the RootElementCollection) which is where ember packets begin.
function emberPayload(unframed) {
  // header for ember frame: 00 0e 00 01 ... ; the BER root starts at 0x60.
  const idx = unframed.indexOf(0x60);
  if (idx < 0) return unframed;
  return unframed.slice(idx);
}

function tagName(cls, tag) {
  if (cls === 'UNIVERSAL') {
    const u = {
      1: 'BOOLEAN', 2: 'INTEGER', 3: 'BITSTRING', 4: 'OCTETSTRING',
      5: 'NULL', 6: 'OID', 9: 'REAL', 10: 'ENUMERATED',
      12: 'UTF8String', 13: 'RELATIVE-OID', 16: 'SEQUENCE', 17: 'SET',
    };
    return u[tag] || `UNIV ${tag}`;
  }
  if (cls === 'APPLICATION') return `[APP ${tag}]`;
  if (cls === 'CONTEXT') return `[${tag}]`;
  return `[PRIV ${tag}]`;
}

function decodeBER(buf, depth, lines, maxDepth, onlyFirst) {
  let off = 0;
  const pad = '  '.repeat(depth);
  while (off < buf.length) {
    const first = buf[off];
    const clsBits = first >> 6;
    const cls = ['UNIVERSAL', 'APPLICATION', 'CONTEXT', 'PRIVATE'][clsBits];
    const constructed = (first & 0x20) !== 0;
    let tag = first & 0x1f;
    let p = off + 1;
    if (tag === 0x1f) {
      // long form tag
      tag = 0;
      while (buf[p] & 0x80) { tag = (tag << 7) | (buf[p] & 0x7f); p++; }
      tag = (tag << 7) | (buf[p] & 0x7f); p++;
    }
    // length
    let len = buf[p];
    p++;
    if (len & 0x80) {
      const n = len & 0x7f;
      len = 0;
      for (let k = 0; k < n; k++) { len = (len << 8) | buf[p]; p++; }
    }
    const content = buf.slice(p, p + len);
    const label = tagName(cls, tag) + (constructed ? ' {' : '');
    if (constructed) {
      lines.push(`${pad}${label}`);
      if (depth < maxDepth) {
        decodeBER(content, depth + 1, lines, maxDepth);
      } else {
        lines.push(`${pad}  ...(${content.length} bytes)`);
      }
      lines.push(`${pad}}`);
    } else {
      let v = '';
      if (cls === 'UNIVERSAL' && tag === 2) v = ` = ${berInt(content)}`;
      else if (cls === 'UNIVERSAL' && tag === 1) v = ` = ${content[0] ? 'true' : 'false'}`;
      else if (cls === 'UNIVERSAL' && tag === 12) v = ` = "${content.toString('utf8')}"`;
      else if (cls === 'UNIVERSAL' && tag === 9) v = ` = ${berReal(content)}`;
      else v = ` = 0x${content.toString('hex')}`;
      lines.push(`${pad}${tagName(cls, tag)}${v}`);
    }
    off = p + len;
    if (onlyFirst) break; // ignore trailing S101 CRC/EOF bytes
  }
}

function berInt(b) {
  if (b.length === 0) return 0;
  let v = b[0] & 0x80 ? -1 : 0;
  for (const byte of b) v = (v * 256) + byte;
  // handle as signed via BigInt-safe small ints
  if (b[0] & 0x80) {
    // two's complement
    let u = 0;
    for (const byte of b) u = u * 256 + byte;
    v = u - Math.pow(256, b.length);
  } else {
    v = 0;
    for (const byte of b) v = v * 256 + byte;
  }
  return v;
}

// Decode a BER REAL content buffer using node-emberplus's own reader.
// We re-frame it as a UNIVERSAL 9 (REAL) TLV and let ExtendedReader.readReal
// reconstruct the IEEE double, matching exactly how the wire was encoded.
function berReal(content) {
  if (content.length === 0) return 0;
  try {
    const tlv = Buffer.concat([
      Buffer.from([EMBER_REAL, content.length]),
      content,
    ]);
    const v = new ExtendedReader(tlv).readReal();
    return Math.round(v * 1000) / 1000;
  } catch (e) {
    return `raw 0x${content.toString('hex')}`;
  }
}

function breakdown(frames, maxDepth = 30) {
  const lines = [];
  frames.forEach((f, i) => {
    const un = s101Unframe(f);
    if (!un) { lines.push(`frame[${i}]: (could not unframe)`); return; }
    const payload = emberPayload(un);
    lines.push(`frame[${i}] BER:`);
    try {
      decodeBER(payload, 1, lines, maxDepth, true);
    } catch (e) {
      lines.push(`  (decode error: ${e.message})`);
    }
  });
  return lines.join('\n');
}

const cat = (frames) => frames.map(hex).join('');

(async () => {
  const client = new EmberClient({ host: HOST, port: PORT });
  await client.connectAsync();
  await client.getDirectoryAsync();
  await wait(200);

  const top = client.root.getChildren()[0];
  await client.getDirectoryAsync(top);
  await wait(200);
  const interop = top.getChildren().find(
    (c) => c.contents && c.contents.identifier === 'interop'
  );
  if (interop == null) throw new Error('interop node (0.4) not found');

  // getDirectory on interop -> returns its child parameters incl enumMap, sliders, offline.
  let r0 = recv.length;
  await client.getDirectoryAsync(interop);
  await wait(400);
  const interopRecv = recv.slice(r0);
  const interopPath = interop.getPath();

  // Find the meters node and expand it (so the streamed params are known).
  const metersNode = interop.getChildren().find(
    (c) => c.contents && c.contents.identifier === 'meters'
  );
  let metersRecv = [];
  if (metersNode) {
    r0 = recv.length;
    await client.getDirectoryAsync(metersNode);
    await wait(300);
    metersRecv = recv.slice(r0);
  }

  // --- Capture a StreamCollection frame. The provider pushes one ~5x/sec to
  // every connected client. We snapshot recv, wait ~1.5s, and pick frames that
  // unframe to a BER payload containing an [APP 5] StreamCollection (tag 0x65).
  r0 = recv.length;
  await wait(1500);
  const streamWindow = recv.slice(r0);
  const streamFrames = streamWindow.filter((f) => {
    const un = s101Unframe(f);
    if (!un) return false;
    const payload = emberPayload(un);
    // APP 5 constructed = 0x60|0x20|0x05 = 0x65 ; StreamCollection BERID.
    return payload.includes(0x65) && !payload.includes(0x6b); // exclude element-collection [APP 11]=0x6b heavy frames
  });

  // Decoded JSON snapshots
  const interopJson = interop.toJSON();

  // ---------------- console ----------------
  console.log(`INTEROP path ${interopPath}, frames=${interopRecv.length}`);
  interopRecv.forEach((b, i) => console.log(`interop.resp[${i}] (${b.length}B): ${hex(b)}`));
  console.log('STREAM frames captured:', streamFrames.length);
  streamFrames.slice(0, 3).forEach((b, i) => console.log(`stream[${i}] (${b.length}B): ${hex(b)}`));

  // ---------------- markdown ----------------
  let md = '# Ember+ Phase 5 wire fixtures (interop node 0.4)\n\n';
  md += 'Captured from a node-emberplus client <-> the provider on 127.0.0.1:9000.\n';
  md += 'All frames are full S101 frames (0xFE ... 0xFF, byte-stuffed, BER payload), hex-encoded.\n';
  md += 'BER tag legend: `[APP n]` = application class, `[n]` = context-specific class.\n\n';

  md += `## 1. interop getDirectory RESPONSE (node "interop", path ${interopPath})\n\n`;
  md += 'Contains the enumMap parameter (0.4.0), tilde-enum (0.4.1), gain slider (0.4.2),\n';
  md += 'level slider (0.4.3), offline node (0.4.4), offline param (0.4.5) and meters node (0.4.6).\n\n';
  interopRecv.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
  md += `Concatenated: \`${cat(interopRecv)}\`\n\n`;
  md += '### Decoded BER tag breakdown\n\n```\n' + breakdown(interopRecv) + '\n```\n\n';
  md += '### Decoded JSON (client.toJSON of interop subtree)\n\n```json\n' +
    JSON.stringify(interopJson, null, 2) + '\n```\n\n';

  if (metersRecv.length) {
    md += '## 2. meters getDirectory RESPONSE (node "meters", path ' + metersNode.getPath() + ')\n\n';
    md += 'Three streamed parameters (meterL id 1, meterR id 2, meterPeak id 3),\n';
    md += 'each carrying streamIdentifier [14].\n\n';
    metersRecv.forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
    md += `Concatenated: \`${cat(metersRecv)}\`\n\n`;
    md += '### Decoded BER tag breakdown\n\n```\n' + breakdown(metersRecv) + '\n```\n\n';
  }

  md += '## 3. StreamCollection frame (provider push)\n\n';
  md += 'The provider has NO public broadcast API in node-emberplus, so it builds a bare\n';
  md += 'root TreeNode, attaches a StreamCollection via TreeNode.setStreams(), and queues it\n';
  md += 'to every connected client ~5x/sec. NOTE: in node-emberplus both StreamCollection and\n';
  md += 'StreamEntry use BERID = APPLICATION(5) (tag 0x65), and each StreamEntry encodes\n';
  md += 'streamIdentifier at [0] (context) and streamValue at [1] (context).\n\n';
  if (streamFrames.length) {
    streamFrames.slice(0, 3).forEach((b, i) => { md += `- frame[${i}] (${b.length} bytes):\n\n      ${hex(b)}\n\n`; });
    md += `Concatenated (first frame): \`${hex(streamFrames[0])}\`\n\n`;
    md += '### Decoded BER tag breakdown (first stream frame)\n\n```\n' +
      breakdown([streamFrames[0]]) + '\n```\n\n';
  } else {
    md += '**No StreamCollection frame captured** (none seen in the capture window).\n\n';
  }

  fs.writeFileSync(path.join(__dirname, 'fixtures_phase5.md'), md);
  console.log('\nWrote fixtures_phase5.md');
  await client.disconnectAsync();
  process.exit(0);
})().catch((e) => { console.error(e.stack); process.exit(1); });
