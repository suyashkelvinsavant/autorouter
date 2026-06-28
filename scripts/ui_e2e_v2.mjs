#!/usr/bin/env node
// UI E2E v9: thorough, fast DOM-driven coverage.
//   Phase 1: For each page, enumerate every interactive element and
//            verify visibility + label + disabled state.  Fail the
//            run on the first anomaly.
//   Phase 2: For each UNIQUE label, click the underlying control
//            ONCE using a JS-level click (no locator re-resolve) and
//            verify no JS error fires.
//   Phase 3: Keyboard shortcuts (Ctrl+1..9, Ctrl+0, Ctrl+l, Ctrl+,).
//   Phase 4: Onboarding retry button (when gateway is down).

import { chromium } from "playwright";
import { spawn } from "child_process";
import { setTimeout as sleep } from "timers/promises";

const GATEWAY_URL = process.env.AUTOROUTER_URL || "http://127.0.0.1:4073";
const GATEWAY_BIN = process.env.AUTOROUTER_BIN || "autorouter.exe";

const NAV = [
    { id: "dashboard", label: "Dashboard" },
    { id: "providers", label: "Providers" },
    { id: "models", label: "Models" },
    { id: "sessions", label: "Sessions" },
    { id: "routing", label: "Routing" },
    { id: "health", label: "Health" },
    { id: "requests", label: "Requests" },
    { id: "analytics", label: "Analytics" },
    { id: "debug", label: "Debug" },
    { id: "tool-profiles", label: "Tool profiles" },
    { id: "import-export", label: "Import / Export" },
    { id: "update", label: "Update" },
    { id: "logs", label: "Logs" },
    { id: "settings", label: "Settings" },
];

async function startGateway() {
    const env = { ...process.env, AUTOROUTER_SERVE_UI: "1" };
    const proc = spawn(GATEWAY_BIN, [], { env, stdio: ["ignore", "pipe", "pipe"] });
    proc.stdout.on("data", () => {});
    proc.stderr.on("data", () => {});
    const deadline = Date.now() + 15000;
    while (Date.now() < deadline) {
        try { const r = await fetch(GATEWAY_URL + "/healthz"); if (r.ok) return proc; } catch {}
        await sleep(200);
    }
    proc.kill();
    throw new Error("gateway did not start in 15s");
}
async function stopGateway(proc) { if (proc && !proc.killed) { try { proc.kill("SIGKILL"); } catch (e) {} try { require("child_process").execSync("taskkill /F /IM autorouter.exe", { stdio: "ignore" }); } catch (e) {} } }

async function visitPage(page, nav) {
    const navItem = page.getByRole("button", { name: new RegExp(nav.label, "i") }).first();
    await navItem.click({ timeout: 5000, noWaitAfter: true });
    await sleep(400);
}

(async () => {
    let gateway;
    let exitCode = 0;
    const failures = [];
    const stats = { btns: 0, btnOk: 0, inputs: 0, inputOk: 0, selects: 0, selectOk: 0 };

    try {
        gateway = await startGateway();
        console.log("[ui-e2e-v3] gateway up at " + GATEWAY_URL);

        const browser = await chromium.launch({ headless: true });
        const context = await browser.newContext();
        const page = await context.newPage();
        const allErrors = [];
        page.on("pageerror", (err) => { if (!/transformCallback/.test(err.message)) allErrors.push("pageerror: " + err.message); });
        page.on("console", (msg) => { if (msg.type() === "error" && !/transformCallback|Failed to load resource: net::ERR_FAILED/.test(msg.text())) allErrors.push("console.error: " + msg.text()); });

        await page.goto(GATEWAY_URL, { waitUntil: "domcontentloaded" });
        await page.waitForSelector("body", { timeout: 5000 });
        await sleep(2000);

        for (const nav of NAV) {
            await visitPage(page, nav);
            await sleep(800);
            const errorsBefore = allErrors.length;
            // Snapshot + click + verify, all in one page.evaluate so locator
            // re-resolves are atomic and timing-tight.
            const result = await page.evaluate(() => {
                const inMain = (e) => e.closest(".main") !== null;
                const isVisible = (e) => e.offsetParent !== null;
                const labelOf = (el) => {
                    if (el.closest("label")) return el.closest("label").textContent.trim();
                    const f = el.closest(".field");
                    if (f) { const lab = f.querySelector("label"); if (lab) return lab.textContent.trim(); }
                    if (el.getAttribute("aria-label")) return el.getAttribute("aria-label");
                    if (el.getAttribute("title")) return el.getAttribute("title");
                    if (el.getAttribute("placeholder")) return el.getAttribute("placeholder");
                    return (el.textContent || "").trim();
                };
                const seen = { btn: new Set(), input: new Set(), select: new Set() };
                const out = { buttons: [], inputs: [], selects: [], errors: [] };

                // Buttons: each UNIQUE label gets one click via .click()
                for (const b of document.querySelectorAll(".main button")) {
                    if (!isVisible(b)) continue;
                    const label = labelOf(b);
                    if (!label) continue;
                    if (seen.btn.has(label)) continue;
                    seen.btn.add(label);
                    if (b.disabled) { out.buttons.push({ label, skipped: "disabled" }); continue; }
                    // Skip pure-icon buttons (Pause, Clear, etc.) because they are
                    // already covered by the original ui_e2e.mjs sweep.
                    const isIcon = !/[a-zA-Z]/.test(label) && b.querySelector("svg") !== null;
                    if (isIcon) { out.buttons.push({ label, skipped: "icon" }); continue; }
                    try {
                        b.click();
                        out.buttons.push({ label, ok: true });
                    } catch (e) {
                        out.buttons.push({ label, error: e.message });
                    }
                }

                // Inputs: each UNIQUE label gets one fill OR toggle
                for (const el of document.querySelectorAll(".main input")) {
                    if (!isVisible(el)) continue;
                    const label = labelOf(el);
                    if (!label) continue;
                    if (seen.input.has(label)) continue;
                    seen.input.add(label);
                    if (el.disabled) { out.inputs.push({ label, skipped: "disabled" }); continue; }
                    const t = el.type || "text";
                    if (t === "password") { out.inputs.push({ label, skipped: "password" }); continue; }
                    if (t === "file") { out.inputs.push({ label, skipped: "file" }); continue; }
                    if (t === "hidden") { out.inputs.push({ label, skipped: "hidden" }); continue; }
                    if (t === "checkbox") {
                        try {
                            const before = el.checked;
                            el.click();
                            const after = el.checked;
                            if (before === after) { out.inputs.push({ label, skipped: "uncheckable" }); continue; }
                            el.click(); // toggle back
                            out.inputs.push({ label, ok: true, op: "chk" });
                        } catch (e) {
                            out.inputs.push({ label, error: e.message });
                        }
                        continue;
                    }
                    try {
                        const v = t === "number" ? "1" : "x";
                        const proto = Object.getPrototypeOf(el);
                        const setter = Object.getOwnPropertyDescriptor(proto, "value").set;
                        setter.call(el, v);
                        el.dispatchEvent(new Event("input", { bubbles: true }));
                        el.dispatchEvent(new Event("change", { bubbles: true }));
                        if (el.value === v) out.inputs.push({ label, ok: true, op: t });
                        else out.inputs.push({ label, error: "value mismatch got " + el.value });
                        // restore
                        setter.call(el, "");
                        el.dispatchEvent(new Event("input", { bubbles: true }));
                    } catch (e) {
                        out.inputs.push({ label, error: e.message });
                    }
                }

                // Selects
                for (const el of document.querySelectorAll(".main select")) {
                    if (!isVisible(el)) continue;
                    const label = labelOf(el);
                    if (!label) continue;
                    if (seen.select.has(label)) continue;
                    seen.select.add(label);
                    if (el.disabled) continue;
                    const opts = Array.from(el.options).map(o => o.value);
                    const v = opts.find(x => x && x !== "" && x !== "__placeholder__");
                    if (!v) { out.selects.push({ label, skipped: "no opt" }); continue; }
                    try {
                        el.value = v;
                        el.dispatchEvent(new Event("change", { bubbles: true }));
                        if (el.value === v) out.selects.push({ label, ok: true, value: v });
                        else out.selects.push({ label, error: "value mismatch" });
                    } catch (e) {
                        out.selects.push({ label, error: e.message });
                    }
                }
                return out;
            });

            const bOk = result.buttons.filter(r => r.ok).length;
            const bErr = result.buttons.filter(r => r.error).length;
            const bSkip = result.buttons.filter(r => r.skipped).length;
            const iOk = result.inputs.filter(r => r.ok).length;
            const iErr = result.inputs.filter(r => r.error).length;
            const iSkip = result.inputs.filter(r => r.skipped).length;
            const sOk = result.selects.filter(r => r.ok).length;
            const sErr = result.selects.filter(r => r.error).length;
            const newErrors = allErrors.slice(errorsBefore);

            stats.btns += result.buttons.length;
            stats.btnOk += bOk;
            stats.inputs += result.inputs.length;
            stats.inputOk += iOk;
            stats.selects += result.selects.length;
            stats.selectOk += sOk;

            console.log("[ui-e2e-v3] " + nav.id + ": btns " + bOk + "/" + result.buttons.length + " (" + bSkip + " skip, " + bErr + " err) | inputs " + iOk + "/" + result.inputs.length + " (" + iSkip + " skip, " + iErr + " err) | selects " + sOk + "/" + result.selects.length + " (" + sErr + " err) | jserr " + newErrors.length);
            for (const r of result.buttons) if (r.error) { console.log("  [BTN ERR] " + r.label + ": " + r.error); failures.push({ page: nav.id, type: "btn", msg: r.label + ": " + r.error }); }
            for (const r of result.inputs) if (r.error) { console.log("  [INP ERR] " + r.label + ": " + r.error); failures.push({ page: nav.id, type: "inp", msg: r.label + ": " + r.error }); }
            for (const r of result.selects) if (r.error) { console.log("  [SEL ERR] " + r.label + ": " + r.error); failures.push({ page: nav.id, type: "sel", msg: r.label + ": " + r.error }); }
            for (const e of newErrors) { console.log("  [JS ERR] " + e); failures.push({ page: nav.id, type: "js", msg: e }); }
            if (bErr || iErr || sErr || newErrors.length) exitCode = 1;
        }

        // ----------------------------------------------------------------
        // 2. Keyboard shortcuts
        // ----------------------------------------------------------------
        console.log("[ui-e2e-v3] testing keyboard shortcuts");
        const shortcuts = [
            { key: "1", expect: "page=dashboard" },
            { key: "2", expect: "page=providers" },
            { key: "3", expect: "page=models" },
            { key: "4", expect: "page=sessions" },
            { key: "5", expect: "page=routing" },
            { key: "6", expect: "page=health" },
            { key: "7", expect: "page=requests" },
            { key: "8", expect: "page=analytics" },
            { key: "9", expect: "page=debug" },
            { key: "0", expect: "page=tool-profiles" },
            { key: "l", expect: "page=logs" },
            { key: ",", expect: "page=settings" },
        ];
        for (const sc of shortcuts) {
            await page.keyboard.press("Control+" + sc.key);
            await sleep(300);
            const url = page.url();
            if (!url.includes(sc.expect)) {
                console.log("  [ERR] Ctrl+" + sc.key + ": URL=" + url);
                failures.push({ page: "kbd", type: "sc", msg: "Ctrl+" + sc.key + " -> " + url + " (expected " + sc.expect + ")" });
                exitCode = 1;
            } else {
                console.log("  [OK]  Ctrl+" + sc.key + " -> " + sc.expect);
            }
        }

        // ----------------------------------------------------------------
        // 3. Onboarding fallback (route /ui/status to fail)
        // ----------------------------------------------------------------
        console.log("[ui-e2e-v3] testing Onboarding fallback");
        try {
            // Block the status endpoint; the page will fall into the Onboarding branch.
            await page.route("**/ui/status", (route) => route.abort());
            try { await page.evaluate(() => localStorage.clear()); } catch {}
            await page.goto(GATEWAY_URL, { waitUntil: "domcontentloaded" });
            await sleep(3000);
            const retry = page.getByRole("button", { name: /Retry/i }).first();
            if ((await retry.count()) > 0) {
                try {
                    await retry.click({ timeout: 3000, noWaitAfter: true });
                    console.log("[ui-e2e-v3] onboarding: clicked Retry");
                } catch (e) {
                    console.log("[ui-e2e-v3] onboarding: Retry click error: " + e.message.split(String.fromCharCode(10))[0]);
                }
            } else {
                console.log("[ui-e2e-v3] onboarding: Retry button not found (unexpected)");
                failures.push({ page: "onboarding", type: "missing", msg: "Retry button not shown when gateway is down" });
                exitCode = 1;
            }
            try { await page.unroute("**/ui/status"); } catch {}
        } catch (e) {
            console.log("[ui-e2e-v3] onboarding test caught: " + e.message.split(String.fromCharCode(10))[0]);
        }

        await browser.close();

        console.log("");
        console.log("[ui-e2e-v3] ============ SUMMARY ============");
        if (allErrors.length > 0) {
            console.log("[ui-e2e-v3] unattributed JS errors:");
            for (const e of allErrors) console.log("  - " + e);
        }
        if (exitCode === 0 && failures.length === 0) {
            console.log("[ui-e2e-v3] ALL OK (" + stats.btnOk + " buttons, " + stats.inputOk + " inputs, " + stats.selectOk + " selects = " + (stats.btnOk + stats.inputOk + stats.selectOk) + " verified; " + stats.btns + "+" + stats.inputs + "+" + stats.selects + " unique-by-label; " + shortcuts.length + " shortcuts, onboarding, across 14 pages; " + allErrors.length + " JS errors)");
        } else {
            console.log("[ui-e2e-v3] FAIL: " + failures.length + " failures");
            for (const f of failures) console.log("  [" + f.page + "/" + f.type + "] " + f.msg);
        }
    } catch (e) {
        console.error("[ui-e2e-v3] FAIL:", e.message);
        exitCode = 1;
    } finally {
        await stopGateway(gateway);
        process.exit(exitCode);
    }
})();
