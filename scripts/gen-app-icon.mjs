/**
 * gen-app-icon.mjs
 *
 * Renders logo_square.svg onto a pitch-black rounded-square background
 * with proper padding (logo fills 75% of the canvas, 12.5% breathing
 * room on every edge) and writes high-resolution icons:
 *
 *   crates/autorouter-desktop/icons/
 *     icon.ico   — multi-size: 16, 24, 32, 48, 64, 128, 256, 512
 *     icon.png   — 1024×1024 master (used by Tauri for Linux / macOS)
 *     icon-bg-32.png    \
 *     icon-bg-128.png    > intermediate sizes for tauri.conf.json icon array
 *     icon-bg-256.png   /
 *
 * All other files in icons/ are left untouched.
 *
 * Usage:
 *   node scripts/gen-app-icon.mjs
 *
 * Requires @resvg/resvg-js (in ui/devDependencies).
 * Run `npm install --prefix ui` once before using this script.
 */

import { readFileSync, writeFileSync, mkdirSync } from 'fs';
import { resolve, dirname }    from 'path';
import { fileURLToPath, pathToFileURL } from 'url';
import { deflateSync }         from 'zlib';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT      = resolve(__dirname, '..');

// Load @resvg/resvg-js from ui/node_modules where it is installed
const resvgEntry = resolve(ROOT, 'ui/node_modules/@resvg/resvg-js/index.js');
const { Resvg } = await import(pathToFileURL(resvgEntry).href);

const SVG_SRC   = resolve(ROOT, 'ui/public/logo_square.svg');
const ICONS_OUT = resolve(ROOT, 'crates/autorouter-desktop/icons');

mkdirSync(ICONS_OUT, { recursive: true });

const svgSource = readFileSync(SVG_SRC, 'utf8');

// ── Design constants ──────────────────────────────────────────────────────────
const LOGO_SCALE          = 0.80;  // logo occupies 80% of canvas (10% padding each side)
const CORNER_RADIUS_RATIO = 0.20;  // rounded-square corner radius as fraction of canvas size
const BG = { r: 0, g: 0, b: 0 };  // pitch black

// ── ICO sizes — all sizes Windows uses, from small to large ──────────────────
// Including 512 so high-DPI Start Menu tiles look crisp.
const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256, 512];

// ── Extra named outputs for tauri.conf.json icon array ───────────────────────
const NAMED_OUTPUTS = [
  { size: 32,   name: 'icon-bg-32.png'  },
  { size: 128,  name: 'icon-bg-128.png' },
  { size: 256,  name: 'icon-bg-256.png' },
  { size: 1024, name: 'icon.png'        }, // master
];

// ─────────────────────────────────────────────────────────────────────────────
// PNG encoder (zero dependencies beyond Node built-ins)
// ─────────────────────────────────────────────────────────────────────────────
function crc32(buf) {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = (c & 1) ? (0xedb88320 ^ (c >>> 1)) : (c >>> 1);
    table[n] = c;
  }
  let crc = 0xffffffff;
  for (const b of buf) crc = (crc >>> 8) ^ table[(crc ^ b) & 0xff];
  return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const lenBuf  = Buffer.alloc(4); lenBuf.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, 'ascii');
  const body    = Buffer.concat([typeBuf, Buffer.isBuffer(data) ? data : Buffer.from(data)]);
  const crcBuf  = Buffer.alloc(4); crcBuf.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([lenBuf, body, crcBuf]);
}

function encodePng(size, rgba) {
  const sig  = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4);
  ihdr.writeUInt8(8, 8);  // bit depth
  ihdr.writeUInt8(6, 9);  // colour type: RGBA
  // Build raw scanlines (filter byte 0 = None per row)
  const stride = size * 4 + 1;
  const raw    = Buffer.alloc(stride * size);
  for (let y = 0; y < size; y++) {
    raw[y * stride] = 0;
    for (let x = 0; x < size; x++) {
      const src = (y * size + x) * 4;
      const dst = y * stride + 1 + x * 4;
      raw[dst]   = rgba[src];
      raw[dst+1] = rgba[src+1];
      raw[dst+2] = rgba[src+2];
      raw[dst+3] = rgba[src+3];
    }
  }
  return Buffer.concat([
    sig,
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', deflateSync(raw, { level: 9 })),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Icon renderer — black rounded-square background + SVG centred with padding
// ─────────────────────────────────────────────────────────────────────────────
function buildIcon(canvasSize) {
  const logoSize = Math.round(canvasSize * LOGO_SCALE);
  const offset   = Math.round((canvasSize - logoSize) / 2);  // equal margin all sides
  const corner   = canvasSize * CORNER_RADIUS_RATIO;

  // Render SVG at the *padded* logo size so every pixel is native resolution
  const resvg    = new Resvg(svgSource, { fitTo: { mode: 'width', value: logoSize } });
  const rendered = resvg.render();
  const logoPixels = rendered.pixels; // Uint8Array RGBA, logoSize×logoSize

  const canvas = new Uint8Array(canvasSize * canvasSize * 4);

  for (let y = 0; y < canvasSize; y++) {
    for (let x = 0; x < canvasSize; x++) {
      const dst = (y * canvasSize + x) * 4;

      // ── Rounded-square mask (outside = fully transparent) ─────────────────
      const cx = canvasSize / 2, cy = canvasSize / 2;
      const ax = Math.max(0, Math.abs(x - cx) - (canvasSize / 2 - corner));
      const ay = Math.max(0, Math.abs(y - cy) - (canvasSize / 2 - corner));
      if (Math.sqrt(ax * ax + ay * ay) > corner) {
        // Transparent outside rounded corners
        canvas[dst] = canvas[dst+1] = canvas[dst+2] = canvas[dst+3] = 0;
        continue;
      }

      // ── Background (pitch black, fully opaque inside rounded square) ──────
      let bgR = BG.r, bgG = BG.g, bgB = BG.b;

      // ── Sample logo pixel (if within logo bounds) ─────────────────────────
      const lx = x - offset;
      const ly = y - offset;
      if (lx >= 0 && lx < logoSize && ly >= 0 && ly < logoSize) {
        const src   = (ly * logoSize + lx) * 4;
        const alpha = logoPixels[src + 3] / 255;
        if (alpha > 0) {
          // Alpha-composite logo over black background
          bgR = Math.round(logoPixels[src]     * alpha + bgR * (1 - alpha));
          bgG = Math.round(logoPixels[src + 1] * alpha + bgG * (1 - alpha));
          bgB = Math.round(logoPixels[src + 2] * alpha + bgB * (1 - alpha));
        }
      }

      canvas[dst]   = bgR;
      canvas[dst+1] = bgG;
      canvas[dst+2] = bgB;
      canvas[dst+3] = 255; // fully opaque
    }
  }

  return encodePng(canvasSize, canvas);
}

// ─────────────────────────────────────────────────────────────────────────────
// Generate all sizes
// ─────────────────────────────────────────────────────────────────────────────
const pngCache = new Map(); // size → Buffer

function getOrBuild(size) {
  if (!pngCache.has(size)) {
    process.stdout.write(`  rendering ${size}×${size}… `);
    pngCache.set(size, buildIcon(size));
    console.log('✓');
  }
  return pngCache.get(size);
}

console.log('\n── Rendering icon sizes ─────────────────────────────────────────────────────');
// Pre-render every size we'll need (ICO + named outputs), deduplicated
const allSizes = [...new Set([...ICO_SIZES, ...NAMED_OUTPUTS.map(o => o.size)])].sort((a, b) => a - b);
for (const s of allSizes) getOrBuild(s);

// Write named PNG outputs
console.log('\n── Writing PNG files ────────────────────────────────────────────────────────');
for (const { size, name } of NAMED_OUTPUTS) {
  const outPath = resolve(ICONS_OUT, name);
  writeFileSync(outPath, getOrBuild(size));
  console.log(`✓ ${name}  (${size}×${size})`);
}

// ── Build icon.ico (multi-size) ───────────────────────────────────────────────
console.log('\n── Building icon.ico ────────────────────────────────────────────────────────');
const icoEntries = ICO_SIZES.map(s => ({ size: s, data: getOrBuild(s) }));
// Calculate offsets
let dataOffset = 6 + icoEntries.length * 16;
for (const e of icoEntries) { e.offset = dataOffset; dataOffset += e.data.length; }

const icoBuf = Buffer.alloc(dataOffset);
icoBuf.writeUInt16LE(0, 0); // reserved
icoBuf.writeUInt16LE(1, 2); // type = 1 (icon)
icoBuf.writeUInt16LE(icoEntries.length, 4);

let pos = 6;
for (const e of icoEntries) {
  // Width/height: 0 means 256 in ICO format; for >256 we also use 0 (PNG entry)
  const w = e.size >= 256 ? 0 : e.size;
  const h = e.size >= 256 ? 0 : e.size;
  icoBuf.writeUInt8(w,  pos);
  icoBuf.writeUInt8(h,  pos + 1);
  icoBuf.writeUInt8(0,  pos + 2); // colour count (0 = not a palette icon)
  icoBuf.writeUInt8(0,  pos + 3); // reserved
  icoBuf.writeUInt16LE(1,  pos + 4); // colour planes
  icoBuf.writeUInt16LE(32, pos + 6); // bits per pixel
  icoBuf.writeUInt32LE(e.data.length, pos + 8);
  icoBuf.writeUInt32LE(e.offset,      pos + 12);
  e.data.copy(icoBuf, e.offset);
  pos += 16;
}

writeFileSync(resolve(ICONS_OUT, 'icon.ico'), icoBuf);
console.log(`✓ icon.ico  (${ICO_SIZES.join(', ')} px  —  ${icoEntries.length} entries)`);

console.log(`
✅  All done.  Output: ${ICONS_OUT}

Next steps:
  1. Ensure tauri.conf.json icon array includes icon-bg-32/128/256.png + icon.ico + icon.icns
  2. Run: \$env:AUTOROUTER_SKIP_SIGNING="1"; node scripts/bundle.mjs
`);
