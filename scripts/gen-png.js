// Minimal zero-dependency PNG encoder (truecolor + alpha).
const fs = require('fs');
const zlib = require('zlib');

function crc32(buf) {
  let c, table = [];
  for (let n = 0; n < 256; n++) {
    c = n;
    for (let k = 0; k < 8; k++) c = (c & 1) ? (0xEDB88320 ^ (c >>> 1)) : (c >>> 1);
    table[n] = c;
  }
  let crc = 0xFFFFFFFF;
  for (let i = 0; i < buf.length; i++) crc = (crc >>> 8) ^ table[(crc ^ buf[i]) & 0xFF];
  return (crc ^ 0xFFFFFFFF) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length, 0);
  const t = Buffer.from(type, 'ascii');
  const td = Buffer.concat([t, data]);
  const c = Buffer.alloc(4); c.writeUInt32BE(crc32(td), 0);
  return Buffer.concat([len, td, c]);
}

function makePng(size, colorFn) {
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr.writeUInt8(8, 8);   // bit depth
  ihdr.writeUInt8(6, 9);   // color type: RGBA
  ihdr.writeUInt8(0, 10);
  ihdr.writeUInt8(0, 11);
  ihdr.writeUInt8(0, 12);
  // raw image data: per-row filter byte (0) + RGBA
  const row = size * 4 + 1;
  const raw = Buffer.alloc(row * size);
  for (let y = 0; y < size; y++) {
    raw[y * row] = 0;
    for (let x = 0; x < size; x++) {
      const [r, g, b, a] = colorFn(x, y, size);
      const off = y * row + 1 + x * 4;
      raw[off] = r; raw[off + 1] = g; raw[off + 2] = b; raw[off + 3] = a;
    }
  }
  const idat = zlib.deflateSync(raw);
  return Buffer.concat([sig, chunk('IHDR', ihdr), chunk('IDAT', idat), chunk('IEND', Buffer.alloc(0))]);
}

function pointSegmentDist(px, py, x1, y1, x2, y2) {
  const dx = x2 - x1, dy = y2 - y1;
  const len2 = dx * dx + dy * dy;
  let t = ((px - x1) * dx + (py - y1) * dy) / (len2 || 1);
  t = Math.max(0, Math.min(1, t));
  const cx = x1 + t * dx, cy = y1 + t * dy;
  return Math.sqrt((px - cx) * (px - cx) + (py - cy) * (py - cy));
}

// Rounded square with an indigo -> violet -> magenta diagonal
// gradient, a stylized white "A" glyph, and a router arc with
// two endpoint dots. The "A" stands for AutoRouter.
const indigo = (x, y, s) => {
  const cx = s / 2, cy = s / 2;
  const dx = x - cx, dy = y - cy;
  const corner = s * 0.22;
  const ax = Math.max(0, Math.abs(dx) - (s / 2 - corner));
  const ay = Math.max(0, Math.abs(dy) - (s / 2 - corner));
  const cornerR = Math.sqrt(ax * ax + ay * ay);
  if (cornerR > corner) return [0, 0, 0, 0];
  const t = (x + y) / (2 * s);
  const r0 = 0x5a, g0 = 0x3d, b0 = 0xf0;
  const r1 = 0xa8, g1 = 0x55, b1 = 0xf7;
  const r2 = 0xec, g2 = 0x48, b2 = 0x99;
  let rr, gg, bb;
  if (t < 0.5) { const k = t / 0.5; rr = r0 + (r1 - r0) * k; gg = g0 + (g1 - g0) * k; bb = b0 + (b1 - b0) * k; }
  else { const k = (t - 0.5) / 0.5; rr = r1 + (r2 - r1) * k; gg = g1 + (g2 - g1) * k; bb = b1 + (b2 - b1) * k; }
  const shade = 1 - 0.18 * Math.max(0, (cornerR - corner * 0.65) / (corner * 0.35));
  rr *= shade; gg *= shade; bb *= shade;
  const u = s / 32;
  const d1 = pointSegmentDist(x, y, 9 * u, 23 * u, 16 * u, 7 * u);
  const d2 = pointSegmentDist(x, y, 16 * u, 7 * u, 23 * u, 23 * u);
  const d3 = pointSegmentDist(x, y, 11 * u, 17 * u, 21 * u, 17 * u);
  if (Math.min(d1, d2, d3) < 1.6 * u) return [255, 255, 255, 255];
  const arcR = 9 * u;
  const arcCx = 16 * u;
  const arcCy = 26 * u + 1.4 * arcR;
  const dist = Math.sqrt((x - arcCx) * (x - arcCx) + (y - arcCy) * (y - arcCy));
  if (Math.abs(dist - arcR) < 1.4 * u && y > 26 * u - u) return [255, 255, 255, 235];
  for (const [dx2, dy2] of [[7 * u, 26 * u], [25 * u, 26 * u]]) {
    if (Math.abs(x - dx2) < 1.8 * u && Math.abs(y - dy2) < 1.8 * u) return [255, 255, 255, 255];
  }
  return [rr | 0, gg | 0, bb | 0, 255];
};

const targets = [
  ['crates/autorouter-desktop/icons/32x32.png', 32],
  ['crates/autorouter-desktop/icons/128x128.png', 128],
  ['crates/autorouter-desktop/icons/128x128@2x.png', 256],
  ['crates/autorouter-desktop/icons/icon.png', 512],
];
for (const [path, size] of targets) {
  fs.mkdirSync(require('path').dirname(path), { recursive: true });
  fs.writeFileSync(path, makePng(size, indigo));
  console.log('wrote', path, size);
}
