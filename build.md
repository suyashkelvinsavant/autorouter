# AutoRouter — Build Reference

> Canonical reference for producing production desktop installers.
> Updated: 2026-06-30

---

## Quick start (Windows — recommended)

```powershell
# From workspace root
$env:AUTOROUTER_SKIP_SIGNING="1"; $env:AUTOROUTER_BUNDLES="nsis"; node scripts/bundle.mjs
```

**Output:** `target\release\bundle\nsis\AutoRouter_0.1.0_x64-setup.exe`

---

## Full command reference

| Scenario | Command |
|---|---|
| NSIS only (recommended on Windows dev machines) | `$env:AUTOROUTER_SKIP_SIGNING="1"; $env:AUTOROUTER_BUNDLES="nsis"; node scripts/bundle.mjs` |
| MSI + NSIS (default, CI/distribution) | `$env:AUTOROUTER_SKIP_SIGNING="1"; node scripts/bundle.mjs` |
| With signing (production release) | `$env:WINDOWS_CERT_FILE="path\to\cert.pfx"; node scripts/bundle.mjs` |

---

## Prerequisites (one-time)

```powershell
npm install --prefix scripts   # installs @tauri-apps/cli@^2.11.3
npm install --prefix ui        # installs React + Vite + @resvg/resvg-js
```

---

## Optional: regenerate tray icons (when SVG changes)

```powershell
node scripts/gen-theme-icons.mjs
# Reads:  ui/public/logo_square.svg
# Writes: crates/autorouter-desktop/icons/tray-{dark,light}.png + @2x variants
```

---

## What `bundle.mjs` does, step by step

1. `npm run build` in `ui/` -> Vite + tsc -> `ui/dist/`
2. Patches `tauri.conf.json` signing fields from env vars (skipped when `AUTOROUTER_SKIP_SIGNING=1`)
3. Runs `node scripts/node_modules/@tauri-apps/cli/tauri.js build --bundles <bundles>`
   from `crates/autorouter-desktop/`
4. Tauri compiles Rust `autorouter-desktop` in `--release` (~3.5 min cold, ~1 min incremental)
5. WiX toolchain -> MSI; NSIS `makensis` -> setup EXE

---

## Output paths

```
target\release\bundle\msi\AutoRouter_0.1.0_x64_en-US.msi      <- WiX (may fail, see below)
target\release\bundle\nsis\AutoRouter_0.1.0_x64-setup.exe      <- NSIS (reliable)
```

---

## Known issues & fixes

### ERROR: `Access is denied. (os error 5)` -- WiX MSI build fails

**Symptom:** WiX `light.exe` succeeds running `candle` but fails writing the `.msi`:
```
Running light to produce ...AutoRouter_0.1.0_x64_en-US.msi
failed to bundle project: `Access is denied. (os error 5)`
```

**Root cause:** On machines where the project is inside an OneDrive-synced folder
(`C:\Users\<user>\OneDrive\...`), the WiX `light.exe` process is denied write access
to the output MSI by Windows (OneDrive/Defender interaction with WiX COM/Windows Installer APIs).
The NSIS pipeline is unaffected.

**Fix (recommended) -- use NSIS only:**
```powershell
$env:AUTOROUTER_BUNDLES="nsis"; $env:AUTOROUTER_SKIP_SIGNING="1"; node scripts/bundle.mjs
```

**Fix -- if you need MSI:**
1. Move the project off OneDrive (e.g. to `C:\dev\autorouter`)
2. OR add `target\` to Windows Defender real-time scan exclusions
3. OR unblock WiX tools (partial fix -- removes Zone.Identifier ADS):
   ```powershell
   Get-ChildItem "$env:LOCALAPPDATA\tauri\WixTools314" -Recurse | Unblock-File
   Get-ChildItem "$env:LOCALAPPDATA\tauri\NSIS" -Recurse | Unblock-File
   ```

The `AUTOROUTER_BUNDLES` env var was added to `scripts/bundle.mjs` (line 42) to allow
bypassing MSI without editing the script.

---

### WARNING: PowerShell `NativeCommandError` -- false alarm

**Symptom:**
```
NativeCommandError: ...node scripts/bundle.mjs 2>&1...
```

**Cause:** PowerShell treats any Node.js output to `stderr` as an error when using `2>&1`.
Tauri CLI prints informational `Info` lines to stderr.

**Not a failure.** Real success indicator = Tauri prints `Finished N bundle(s) at:`.

---

### WARNING: WiX tools re-download on every run

**Symptom:** `Downloading https://go.microsoft.com/fwlink/p/?LinkId=2124703` appears on
every build even though `%LOCALAPPDATA%\tauri\WixTools314` exists.

**Cause:** That URL is the **WebView2 bootstrapper**, not WiX tools. WiX tools ARE
cached in `WixTools314`. The WebView2 download is a normal Tauri bundler step.

---

## Env vars

| Variable | Default | Purpose |
|---|---|---|
| `AUTOROUTER_SKIP_SIGNING` | unset | Set to `1` to skip code signing |
| `AUTOROUTER_BUNDLES` | `msi,nsis` (win) / `app,dmg` (mac) / `deb,appimage` (linux) | Override `--bundles` flag passed to Tauri CLI |
| `WINDOWS_CERT_FILE` | unset | Path to `.pfx` for Windows code signing |
| `APPLE_ID` | unset | Apple ID for macOS notarisation |
| `APPLE_TEAM_ID` | unset | Apple Team ID for macOS notarisation |

---

## Tauri tools cache locations

| Tool | Path |
|---|---|
| WiX 3.14 binaries | `%LOCALAPPDATA%\tauri\WixTools314\` |
| NSIS | `%LOCALAPPDATA%\tauri\NSIS\` |
| WebView2 bootstrapper | `%LOCALAPPDATA%\tauri\MicrosoftEdgeWebview2Setup.exe` |

---

## Build history

| Date | Build | Notes |
|------|-------|-------|
| 2026-06-23 | #1 | Baseline -- first successful production build |
| 2026-06-24 | #2 | Added `gen-theme-icons.mjs`, `@resvg/resvg-js`, `lib.rs` updates |
| 2026-06-30 | #3 | Diagnosed WiX `Access is denied` on OneDrive path; added `AUTOROUTER_BUNDLES` env override to `bundle.mjs`; confirmed NSIS-only path works reliably |
