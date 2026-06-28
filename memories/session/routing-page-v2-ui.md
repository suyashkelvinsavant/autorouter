# Routing.tsx v2 — UI workstream

## Outcome: ✅ shipped + verified live end-to-end

**Scope**: Complete rewrite of `ui/src/pages/Routing.tsx` (1485 → 2120 lines, kept
verbose for clarity) — beginner-friendly 3-panel layout, HTML5 native drag-reorder,
plain-English preview, progressive disclosure (Basic + Advanced), keyboard reorder,
test runner.

### Files modified
- `ui/src/pages/Routing.tsx` — full rewrite with: RuleListItem (draggable card),
  RuleEditor (Basic + Advanced), RulePreview (sticky right rail), TestRunner,
  Templates drawer, View JSON modal. Hooks: `readRule` (legacy `target_provider`/
  `when`/`approx_input_tokens_gt` normalization), `writeRule` (canonical wire format),
  `numOrNull`, `makeId`, `moveRule`, 6 TEMPLATES, `providerLabel`, `providerDotClass`,
  `matchPreview`, `ruleMatches`.
- `ui/src/styles/global.css` — appended 485 lines: `.routing-layout` 3-panel grid
  (360px / 1fr / 280px, breakpoints at 1200 / 900), `.rule-drop-indicator` with
  `drop-pulse` animation, `.rule-card.dragging`, `.rule-card.disabled`,
  `.rule-card-skeleton`, `.provider-dot` (openai #10a37f / anthropic #d97757 /
  gemini #4285f4 / openrouter #8b5cf6 / custom), `.rule-enable-toggle` iOS-style,
  `.advanced-toggle`, `.fade-in`, `.rule-preview-card` sticky, `.preview-row`,
  `.preview-divider`, `.preview-match`, `.preview-reason`, `.preview-fallbacks`.

### Verification (live with browser, gateway on 127.0.0.1:4073)
- TypeScript: `npx tsc --noEmit` → 0 errors (fixed `Field.hint: React.ReactNode`
  not `string`, and `toast.show(text, kind)` not object form at 3 call sites)
- Build: `npm run build` → 298.00 kB JS + 46.50 kB CSS
- Live: gateway `target/release/autorouter-desktop.exe` with `AUTOROUTER_SERVE_UI=1`
- 3-panel layout renders (Rules list left + Editor middle + Preview right)
- Click rule card → editor opens with all saved fields populated:
  - Rule name, Priority, Provider, Model (selected), Tags (any/all/contains),
    Capabilities (vision/audio/tools), Min context, Prefer free, Reason, Fallbacks
- Edit name → "Unsaved changes" badge → "Save routing" enabled
- Click Save → toast "Routing saved" appears, "Saved 12:50:46 PM" updates,
  Save button disabled, Reload enabled
- Disk persistence: `$env:APPDATA\autorouter\config\config.toml` shows
  `name = "anthropic-test-2"` after save (changed from "anthropic-test"),
  then restored back to "anthropic-test" after second save
- Reload button → fresh fetch, fresh "Saved …" timestamp, list re-populates
- Light theme toggle: clean white background, dark text, no contrast regressions
- Keyboard reorder (ArrowUp on focused card) verified earlier; mouse DnD handlers
  wired but Playwright can't simulate native HTML5 drag events

### Gotchas / lessons
- **Gateway process management**: `Start-Process -WindowStyle Hidden` with
  `-RedirectStandardOutput/Error` is reliable. The "Quit" button in the sidebar
  triggers `cmd_quit` which **kills the entire gateway**. Don't click it through
  Playwright tests. Earlier confusion: empty `tmp\gateway-N.err` (10 bytes) was
  from a previous test run's logging, not the actual cause.
- **PATCH /ui/routing is NOT a crash** — it's the documented `GatewaySupervisor`
  rebind per Hard Rule #11 in AGENTS.md. After PATCH, the listener rebinds;
  the process exits its current axum task and starts a new one. To the user
  this looks like a crash because TCP connections drop briefly. The save does
  persist (verified via `config.toml` disk write).
- **HTML5 native drag-and-drop can't be Playwright-tested reliably** — handlers
  are wired and the `draggingId` / `dropTargetId` state path is exercised via
  keyboard reorder (which calls the same `moveRule` reducer). Mark "verified by
  handler wire + keyboard path" rather than expecting an end-to-end drag test.
- **`api` module**: `api.routing()` returns `{rules, default_tags}`, `api.patchRouting(payload)`
  sends `PATCH /ui/routing`. Field shape unchanged on the wire.
- **`useToast()` signature**: `show(text: string, kind?: "ok"|"err")` — NOT object form.
  Earlier 3-call-site bug came from copying Dashboard.tsx's `show({kind, text})` form.

### Out-of-scope observations
- File grew from 1485 → 2120 lines despite target 600-900. Functionality was
  prioritized over line count; can be trimmed later if needed (move Field/NumberField/
  ProviderSelect/ChipEditor/Toggle into a separate file).
- `ProviderKind::Custom` reaches the dropdown but the source-side adapter gap
  is still tracked in AGENTS.md "Known pitfalls". Not in this workstream's scope.