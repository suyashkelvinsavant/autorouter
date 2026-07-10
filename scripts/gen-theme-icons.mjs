/**
 * gen-theme-icons.mjs
 *
 * Generates theme-responsive tray PNG icons from the project's logo_square.svg.
 *
 * Outputs (into crates/autorouter-desktop/icons/):
 *   tray-dark.png   — 32×32, white icon on transparent bg  (used in OS dark mode)
 *   tray-light.png  — 32×32, black icon on transparent bg  (used in OS light mode)
 *   tray-dark@2x.png   — 64×64 Retina variants
 *   tray-light@2x.png
 *
 * The SVG source (logo_square.svg) uses fill="#ffffff" (white paths).
 * For the light-mode icon we swap the fill to #000000 (black).
 *
 * Usage:
 *   node scripts/gen-theme-icons.mjs
 */

import { readFileSync, writeFileSync, mkdirSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import { Resvg } from '@resvg/resvg-js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, '..');

// Paths
const SVG_SRC   = resolve(ROOT, 'ui/public/logo_square.svg');
const ICONS_OUT = resolve(ROOT, 'crates/autorouter-desktop/icons');

mkdirSync(ICONS_OUT, { recursive: true });

// Read the original SVG (paths are white — fill="#ffffff")
const svgWhite = readFileSync(SVG_SRC, 'utf8');

// Create a black-fill variant by replacing the root fill attribute
const svgBlack = svgWhite.replace(/fill="#ffffff"/i, 'fill="#000000"');

/**
 * Render an SVG string to a PNG Buffer at the given output size.
 * resvg-js scales the SVG to fit the requested pixel dimensions.
 */
function renderSvgToPng(svgString, size) {
  const resvg = new Resvg(svgString, {
    fitTo: {
      mode: 'width',
      value: size,
    },
    // Keep background transparent
    background: undefined,
  });
  const pngData = resvg.render();
  return pngData.asPng();
}

// ── Generate icons ────────────────────────────────────────────────────────────

// dark mode tray icon: white icon (visible against dark taskbar/menu bar)
const darkPng   = renderSvgToPng(svgWhite, 32);
const dark2xPng = renderSvgToPng(svgWhite, 64);

// light mode tray icon: black icon (visible against light taskbar/menu bar)
const lightPng   = renderSvgToPng(svgBlack, 32);
const light2xPng = renderSvgToPng(svgBlack, 64);

writeFileSync(resolve(ICONS_OUT, 'tray-dark.png'),    darkPng);
writeFileSync(resolve(ICONS_OUT, 'tray-dark@2x.png'), dark2xPng);
writeFileSync(resolve(ICONS_OUT, 'tray-light.png'),   lightPng);
writeFileSync(resolve(ICONS_OUT, 'tray-light@2x.png'),light2xPng);

console.log('✓ tray-dark.png    (32×32, white)');
console.log('✓ tray-dark@2x.png (64×64, white)');
console.log('✓ tray-light.png   (32×32, black)');
console.log('✓ tray-light@2x.png(64×64, black)');
console.log(`\nAll icons written to: ${ICONS_OUT}`);
