const fs = require('fs');
const path = require('path');

const iconsDir = 'crates/autorouter-desktop/icons';
const candidates = [16, 32, 48, 64, 128, 256];
const pngs = [];
for (const s of candidates) {
  const p = path.join(iconsDir, s + 'x' + s + '.png');
  if (fs.existsSync(p)) pngs.push({ size: s, data: fs.readFileSync(p) });
}
if (pngs.length === 0) {
  console.error('no source PNGs found in', iconsDir);
  process.exit(1);
}

const headerLen = 6 + pngs.length * 16;
let total = headerLen;
const entries = pngs.map(p => {
  const e = { size: p.size, data: p.data, offset: total };
  total += p.data.length;
  return e;
});
const buf = Buffer.alloc(total);
buf.writeUInt16LE(0, 0);
buf.writeUInt16LE(1, 2);
buf.writeUInt16LE(entries.length, 4);
let pos = 6;
for (const e of entries) {
  const w = e.size >= 256 ? 0 : e.size;
  const h = e.size >= 256 ? 0 : e.size;
  buf.writeUInt8(w, pos);
  buf.writeUInt8(h, pos + 1);
  buf.writeUInt8(0, pos + 2);
  buf.writeUInt8(0, pos + 3);
  buf.writeUInt16LE(1, pos + 4);
  buf.writeUInt16LE(32, pos + 6);
  buf.writeUInt32LE(e.data.length, pos + 8);
  buf.writeUInt32LE(e.offset, pos + 12);
  e.data.copy(buf, e.offset);
  pos += 16;
}
fs.writeFileSync(path.join(iconsDir, 'icon.ico'), buf);
console.log('wrote icon.ico with', entries.length, 'entries');

// Build multi-entry .icns
function entry(type, png) {
  const head = Buffer.alloc(8);
  head.write(type, 0, 'ascii');
  head.writeUInt32BE(8 + png.length, 4);
  return Buffer.concat([head, png]);
}
const png128 = fs.readFileSync(path.join(iconsDir, '128x128.png'));
const parts = [entry('ic07', png128)];
try {
  const png256 = fs.existsSync(path.join(iconsDir, '256x256.png')) ? fs.readFileSync(path.join(iconsDir, '256x256.png')) : null;
  if (png256) parts.push(entry('ic08', png256));
} catch (e) {}
try {
  const png512 = fs.existsSync(path.join(iconsDir, '512x512.png')) ? fs.readFileSync(path.join(iconsDir, '512x512.png')) : null;
  if (png512) parts.push(entry('ic09', png512));
} catch (e) {}
const icnsHead = Buffer.alloc(8);
icnsHead.write('icns', 0, 'ascii');
const bodyLen = parts.reduce((a, b) => a + b.length, 0);
icnsHead.writeUInt32BE(8 + bodyLen, 4);
const icns = Buffer.concat([icnsHead, ...parts]);
fs.writeFileSync(path.join(iconsDir, 'icon.icns'), icns);
console.log('wrote icon.icns with', parts.length, 'entries');
