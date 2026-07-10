#!/usr/bin/env node
/**
 * Build platform-specific installers for AutoRouter.
 *
 * On Windows, produces both NSIS (.exe) and WiX (.msi) installers.
 * On macOS, produces the .app bundle and the .dmg disk image.
 * On Linux, produces the .deb and the AppImage.
 *
 * Usage:
 *   node scripts/bundle.mjs
 *
 * The script reads the Tauri CLI from scripts/node_modules; install
 * it with `npm install --prefix scripts @tauri-apps/cli@^2` the
 * first time.
 */

import { spawnSync } from "node:child_process";
import { existsSync, statSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import os from "node:os";

import { fileURLToPath } from "node:url";
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "..");
const cli = path.join(root, "scripts", "node_modules", "@tauri-apps", "cli", "tauri.js");

if (!existsSync(cli)) {
  console.error(
    "Tauri CLI not found. Install it with:\n" +
      "  npm install --prefix scripts @tauri-apps/cli@^2"
  );
  process.exit(1);
}

const platform = os.platform();
let bundles = "all";
if (platform === "win32") bundles = "msi,nsis";
else if (platform === "darwin") bundles = "app,dmg";
else if (platform === "linux") bundles = "deb,appimage";
// Allow the caller to override the bundle list without editing this file.
// Example: $env:AUTOROUTER_BUNDLES="nsis"; node scripts/bundle.mjs
if (process.env.AUTOROUTER_BUNDLES) bundles = process.env.AUTOROUTER_BUNDLES;

console.log(`[bundle] target=${platform} bundles=${bundles}`);

// Pre-build the UI so the Tauri bundler can pick up dist/.
// We do it explicitly (rather than via tauri.conf.json
// beforeBuildCommand) so the path resolution works the same on
// Windows, macOS, and Linux.
const uiDir = path.join(root, "ui");
const distDir = path.join(uiDir, "dist");
if (!existsSync(path.join(uiDir, "package.json"))) {
  console.error("[bundle] UI directory not found at " + uiDir);
  process.exit(1);
}
if (!existsSync(path.join(uiDir, "node_modules"))) {
  console.log("[bundle] installing UI dependencies...");
  const inst = spawnSync("npm", ["install", "--no-fund", "--no-audit"], {
    cwd: uiDir,
    stdio: "inherit",
    shell: true,
  });
  if (inst.status !== 0) process.exit(inst.status ?? 1);
}
console.log("[bundle] building UI...");
const ui = spawnSync("npm", ["run", "build"], {
  cwd: uiDir,
  stdio: "inherit",
  shell: true,
});
if (ui.status !== 0) process.exit(ui.status ?? 1);

// Sanity check the dist directory exists and has at least one
// file before we hand off to Tauri.
if (!existsSync(distDir) || !statSync(distDir).isDirectory()) {
  console.error("[bundle] UI dist directory missing after build");
  process.exit(1);
}
console.log(`[bundle] UI dist ok (${distDir})`);

// M19: template tauri.conf.json signing fields from the
// environment. AUTOROUTER_SKIP_SIGNING=1 explicitly opts out of
// signing; otherwise we forward APPLE_ID/APPLE_PASSWORD/APPLE_TEAM_ID
// and Windows cert vars to Tauri's bundler.
const confPath = path.join(root, "crates", "autorouter-desktop", "tauri.conf.json");
const skipSigning = process.env.AUTOROUTER_SKIP_SIGNING === "1";
if (!skipSigning && existsSync(confPath)) {
  const conf = JSON.parse(readFileSync(confPath, "utf8"));
  if (process.env.WINDOWS_CERT_FILE) {
    conf.bundle.windows.certificateThumbprint = null;
    conf.bundle.windows.timestampUrl = process.env.WINDOWS_CERT_FILE;
  }
  if (process.env.APPLE_ID) {
    conf.bundle.macOS.signingIdentity = process.env.APPLE_ID;
  }
  if (process.env.APPLE_TEAM_ID) {
    conf.bundle.macOS.providerShortName = process.env.APPLE_TEAM_ID;
  }
  writeFileSync(confPath, JSON.stringify(conf, null, 2) + "\n");
  console.log("[bundle] signing fields templated from env");
} else if (skipSigning) {
  console.log("[bundle] AUTOROUTER_SKIP_SIGNING=1; skipping signing");
}

const env = { ...process.env };
if (skipSigning) {
  // Tauri picks up these env vars too; setting them to "" disables signing.
  env.CSC_IDENTITY_AUTO_DISCOVERY = "false";
}

console.log(`[bundle] invoking Tauri CLI: node ${cli} build --bundles ${bundles}`);
const r = spawnSync("node", [cli, "build", "--bundles", bundles], {
  cwd: path.join(root, "crates", "autorouter-desktop"),
  stdio: "inherit",
  shell: true,
  env,
});
process.exit(r.status ?? 1);
