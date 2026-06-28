// Convenience script for local development: install the UI deps
// if needed, build the UI, then invoke the Tauri bundler in
// release mode. On Windows, this produces the .exe + MSI + NSIS
// outputs. On macOS, it produces the .app + .dmg. On Linux, it
// produces the .deb + .AppImage.

const { spawnSync } = require("child_process");
const path = require("path");
const fs = require("fs");

const root = path.resolve(__dirname, "..");
const uiDir = path.join(root, "ui");

function run(cmd, args, cwd) {
  const r = spawnSync(cmd, args, { cwd, stdio: "inherit", shell: true });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

if (!fs.existsSync(path.join(uiDir, "node_modules"))) {
  console.log("[rebuild] installing UI dependencies...");
  run("npm", ["install", "--no-fund", "--no-audit"], uiDir);
}

console.log("[rebuild] building UI...");
run("npm", ["run", "build"], uiDir);

const cliPath = path.resolve(root, "scripts/node_modules/@tauri-apps/cli/tauri.js");
if (!fs.existsSync(cliPath)) {
  console.error(
    "Tauri CLI not found. Install it with:\n" +
      "  npm install --prefix scripts @tauri-apps/cli@^2"
  );
  process.exit(1);
}

console.log("[rebuild] invoking Tauri CLI...");
const r = spawnSync("node", [cliPath, "build"], {
  cwd: path.join(root, "crates", "autorouter-desktop"),
  stdio: "inherit",
  shell: true,
});
process.exit(r.status ?? 1);
