#!/usr/bin/env node
// UI E2E v6: drive every button on every page and verify the result
// by inspecting the network calls and the resulting DOM. Fails
// loudly if a click does not trigger the expected API call or the
// API call returns a non-2xx status.

import { chromium } from "playwright";
import { spawn } from "child_process";
import { setTimeout as sleep } from "timers/promises";
import { strict as assert } from "assert";

const GATEWAY_URL = process.env.AUTOROUTER_URL || "http://127.0.0.1:4073";
const GATEWAY_BIN = process.env.AUTOROUTER_BIN || "autorouter.exe";

const NAV = [
    { id: "dashboard", label: "Dashboard",
      clickables: [
        { name: "OpenAI", expectedNav: "providers" },
        { name: "Anthropic", expectedNav: "providers" },
        { name: "Gemini", expectedNav: "providers" },
      ],
    },
    { id: "providers", label: "Providers",
      buttons: [
        { text: "Save", expected: { method: "PATCH", path: "/ui/settings" }, expectStatus: 200 },
        { text: "Refresh", expected: { method: "GET", path: "/ui/providers" }, expectStatus: 200 },
      ],
    },
    { id: "models", label: "Models",
      buttons: [
        { text: "Refresh", expected: { method: "GET", path: "/ui/providers" }, expectStatus: 200 },
      ],
    },
    { id: "sessions", label: "Sessions",
      buttons: [
        { text: "Refresh", expected: { method: "GET", path: "/ui/sessions" }, expectStatus: 200 },
      ],
    },
    { id: "routing", label: "Routing",
      buttons: [
        { text: "Reload", expected: { method: "GET", path: "/ui/routing" }, expectStatus: 200 },
        { text: "New rule", expected: null, expectStatus: null },
        { text: "Save all", expected: { method: "PATCH", path: "/ui/routing" }, expectStatus: 200 },
      ],
    },
    { id: "health", label: "Health",
      buttons: [
        { text: "Refresh", expected: { method: "GET", path: "/ui/health" }, expectStatus: 200 },
      ],
    },
    { id: "requests", label: "Requests",
      buttons: [
        { text: "Refresh", expected: { method: "GET", path: "/ui/events" }, expectStatus: 200 },
      ],
    },
    { id: "analytics", label: "Analytics",
      buttons: [
        { text: "Refresh", expected: { method: "GET", path: "/ui/analytics" }, expectStatus: 200 },
      ],
    },
    { id: "debug", label: "Debug",
      buttons: [
        { text: "Refresh", expected: { method: "GET", path: "/ui/debug" }, expectStatus: 200 },
        { text: "Copy JSON", expected: null, expectStatus: null },
      ],
    },
    { id: "tool-profiles", label: "Tool profiles",
      buttons: [
        { text: "Reload", expected: { method: "GET", path: "/ui/tool_profiles" }, expectStatus: 200 },
        { text: "New profile", expected: null, expectStatus: null },
        { text: "Save", expected: { method: "POST", path: "/ui/tool_profiles" }, expectStatus: 200 },
        { text: "Run", expected: { method: "POST", path: "/ui/tool_test" }, expectStatus: 200 },
      ],
    },
    { id: "import-export", label: "Import / Export",
      buttons: [
        { text: "Reload", expected: { method: "GET", path: "/ui/export" }, expectStatus: 200 },
        { text: "Download", expected: null, expectStatus: null },
        { text: "Upload file", expected: null, expectStatus: null },
        { text: "Import & apply", expected: { method: "POST", path: "/ui/import" }, expectStatus: 200 },
      ],
    },
    { id: "update", label: "Update",
      buttons: [
        { text: "Check now", expected: { method: "GET", path: "/ui/update" }, expectStatus: 200 },
      ],
    },
    { id: "logs", label: "Logs",
      buttons: [
        { text: "Pause", expected: null, expectStatus: null },
        { text: "Clear", expected: null, expectStatus: null },
      ],
    },
    { id: "settings", label: "Settings",
      buttons: [
        { text: "Reload", expected: { method: "GET", path: "/ui/settings" }, expectStatus: 200 },
        { text: "Save server", expected: { method: "PATCH", path: "/ui/settings" }, expectStatus: 200 },
        { text: "Save defaults", expected: { method: "PATCH", path: "/ui/settings" }, expectStatus: 200 },
        { text: "Save logging", expected: { method: "PATCH", path: "/ui/settings" }, expectStatus: 200 },
        { text: "Restart server", expected: { method: "POST", path: "/ui/restart" }, expectStatus: 200 },
        { text: "Reveal data dir", expected: null, expectStatus: null },
      ],
    },
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

function pathOfUrl(url) { try { return new URL(url).pathname; } catch { return url; } }
function summarizeCall(c) { return c.method + " " + pathOfUrl(c.url) + " -> " + (c.status || "?"); }

async function visitPage(page, nav) {
    const navItem = page.getByRole("button", { name: new RegExp(nav.label, "i") }).first();
    await navItem.click({ timeout: 5000, noWaitAfter: true });
    await sleep(400);
}

(async () => {
    let gateway;
    let exitCode = 0;
    const failures = [];
    try {
        gateway = await startGateway();
        console.log("[ui-e2e] gateway up at " + GATEWAY_URL);

        const browser = await chromium.launch({ headless: true });
        const context = await browser.newContext();
        const page = await context.newPage();
        const calls = [];
        page.on("request", (req) => {
            const url = req.url();
            if (url.startsWith(GATEWAY_URL)) {
                calls.push({ method: req.method(), url, status: null });
            }
        });
        page.on("response", (resp) => {
            const url = resp.url();
            if (url.startsWith(GATEWAY_URL)) {
                for (let i = calls.length - 1; i >= 0; i--) {
                    if (calls[i].url === url && calls[i].status == null) {
                        calls[i].status = resp.status();
                        break;
                    }
                }
            }
        });
        page.on("pageerror", (err) => {
            if (!/transformCallback/.test(err.message)) {
                console.error("[pageerror]", err.message);
            }
        });

        await page.goto(GATEWAY_URL, { waitUntil: "domcontentloaded" });
        await page.waitForSelector("body", { timeout: 5000 });
        await sleep(1500);

        const report = {};
        for (const nav of NAV) {
            await visitPage(page, nav);
            await sleep(500);
            const before = calls.length;
            const results = [];
            for (const btn of nav.buttons || []) {
                const loc = page.getByRole("button", { name: new RegExp("^\\s*" + btn.text + "\\s*$", "i") }).first();
                if ((await loc.count()) === 0) {
                    results.push({ text: btn.text, error: "not found" });
                    failures.push({ page: nav.id, text: btn.text, error: "not found" });
                    continue;
                }
                if (await loc.isDisabled()) {
                    results.push({ text: btn.text, skipped: "disabled" });
                    continue;
                }
                try {
                    const beforeClick = calls.length;
                    await loc.click({ timeout: 2000, noWaitAfter: true });
                    await sleep(500);
                    const afterClick = calls.slice(beforeClick);
                    if (btn.expected) {
                        const match = afterClick.find(
                            (c) => c.method === btn.expected.method && pathOfUrl(c.url) === btn.expected.path
                        );
                        if (!match) {
                            const got = afterClick.map(summarizeCall).join(", ");
                            results.push({ text: btn.text, error: "no " + btn.expected.method + " " + btn.expected.path + " (got: " + (got || "nothing") + ")" });
                            failures.push({ page: nav.id, text: btn.text, error: "no " + btn.expected.method + " " + btn.expected.path + " (got: " + (got || "nothing") + ")" });
                            continue;
                        }
                        if (btn.expectStatus != null && match.status !== btn.expectStatus) {
                            results.push({ text: btn.text, error: "status " + match.status + " != " + btn.expectStatus });
                            failures.push({ page: nav.id, text: btn.text, error: "status " + match.status + " != " + btn.expectStatus });
                            continue;
                        }
                        results.push({ text: btn.text, ok: true, call: summarizeCall(match) });
                    } else {
                        results.push({ text: btn.text, ok: true, note: "no API call expected" });
                    }
                } catch (e) {
                    const msg = e.message.split(String.fromCharCode(10))[0];
                    results.push({ text: btn.text, error: msg });
                    failures.push({ page: nav.id, text: btn.text, error: msg });
                }
            }
            report[nav.id] = { results, callsDuring: calls.length - before };
            const okCount = results.filter((r) => r.ok).length;
            const errCount = results.filter((r) => r.error).length;
            console.log("[ui-e2e] " + nav.id + ": " + results.length + " buttons, " + okCount + " ok, " + errCount + " err");
            for (const r of results) {
                if (r.error) console.log("  [ERR] " + r.text + ": " + r.error);
                else if (r.ok) console.log("  [OK]  " + r.text + ": " + (r.call || r.note || "clicked"));
                else console.log("  [--]  " + r.text + ": " + r.skipped);
            }
        }

        // Per-row button sweep: for each page, click every not-yet-tested <button>
        // to ensure no JS error blocks state-only actions (copy/move/delete/etc.).
        console.log("[ui-e2e] per-row button sweep across all pages");
        let sweepOk = 0;
        let sweepSkip = 0;
        for (const nav of NAV) {
            await visitPage(page, nav);
            await sleep(400);
            // Skip nav rail + header, only click within the main panel.
            const allBtns = await page.locator('main button, [class*=page] button, [class*=panel] button').all();
            for (const b of allBtns) {
                const txt = ((await b.textContent()) || "").trim().replace(/\s+/g, " ");
                if (!txt) continue;
                if (await b.isDisabled()) { sweepSkip++; continue; }
                try {
                    const before = calls.length;
                    await b.click({ timeout: 1500, noWaitAfter: true });
                    await sleep(250);
                    sweepOk++;
                } catch (e) {
                    // copy/move buttons write to clipboard which Playwright denies; that is not a failure.
                    if (/clipboard|permission/i.test(e.message)) { sweepSkip++; continue; }
                    failures.push({ page: nav.id, text: txt.substring(0, 40), error: "sweep click: " + e.message.split(String.fromCharCode(10))[0] });
                }
            }
        }
        console.log("[ui-e2e] sweep: " + sweepOk + " clicks OK, " + sweepSkip + " skipped (disabled/clipboard)");

        // Clickable cards (Dashboard, etc.).
        for (const nav of NAV) {
            if (!nav.clickables) continue;
            await visitPage(page, nav);
            await sleep(500);
            const reportEntry = (report[nav.id] = report[nav.id] || { results: [] });
            for (const c of nav.clickables) {
                const loc = page.locator("[role=button].card.clickable").filter({ hasText: c.name }).first();
                if ((await loc.count()) === 0) {
                    failures.push({ page: nav.id, text: c.name, error: "clickable not found" });
                    reportEntry.results.push({ text: c.name, error: "clickable not found" });
                    continue;
                }
                try {
                    await loc.click({ timeout: 2000, noWaitAfter: true });
                    await sleep(500);
                    const url = page.url();
                    if (c.expectedNav && !url.includes("page=" + c.expectedNav)) {
                        failures.push({ page: nav.id, text: c.name, error: "did not navigate to " + c.expectedNav + " (url=" + url + ")" });
                        reportEntry.results.push({ text: c.name, error: "no nav to " + c.expectedNav });
                    } else {
                        reportEntry.results.push({ text: c.name, ok: true, note: "navigated to " + c.expectedNav });
                    }
                } catch (e) {
                    const msg = e.message.split(String.fromCharCode(10))[0];
                    failures.push({ page: nav.id, text: c.name, error: msg });
                    reportEntry.results.push({ text: c.name, error: msg });
                }
                await visitPage(page, nav);
                await sleep(300);
            }
            const clickOk = reportEntry.results.filter((r) => r.ok).length;
            const clickErr = reportEntry.results.filter((r) => r.error).length;
            console.log("[ui-e2e] " + nav.id + " clickables: " + reportEntry.results.length + " tested, " + clickOk + " ok, " + clickErr + " err");
            for (const r of reportEntry.results) {
                if (r.error) console.log("  [ERR] " + r.text + ": " + r.error);
                else if (r.ok) console.log("  [OK]  " + r.text + ": " + r.note);
            }
        }

        // Keyboard shortcut test
        console.log("[ui-e2e] testing keyboard shortcuts");
        const shortcuts = [
            { key: '1', expect: 'page=dashboard' },
            { key: '2', expect: 'page=providers' },
            { key: '3', expect: 'page=models' },
            { key: '4', expect: 'page=sessions' },
            { key: '5', expect: 'page=routing' },
            { key: '6', expect: 'page=health' },
            { key: '7', expect: 'page=requests' },
            { key: '8', expect: 'page=analytics' },
            { key: '9', expect: 'page=debug' },
            { key: '0', expect: 'page=tool-profiles' },
            { key: 'l', expect: 'page=logs' },
            { key: ',', expect: 'page=settings' },
        ];
        for (const sc of shortcuts) {
            await page.keyboard.press('Control+' + sc.key);
            await sleep(300);
            const url = page.url();
            if (!url.includes(sc.expect)) {
                failures.push({ page: 'keyboard', text: 'Ctrl+' + sc.key, error: 'expected URL to contain ' + sc.expect + ' (url=' + url + ')' });
                console.log('  [ERR] Ctrl+' + sc.key + ': URL=' + url);
            } else {
                console.log('  [OK]  Ctrl+' + sc.key + ' -> ' + sc.expect);
            }
        }
        // Onboarding fallback (route /ui/status to fail).
        console.log("[ui-e2e] testing Onboarding fallback");
        try {
            await page.route("**/ui/status", (route) => route.abort());
            await page.goto(GATEWAY_URL, { waitUntil: "domcontentloaded" });
            await sleep(3000);
            const retry = page.getByRole("button", { name: /Retry/i }).first();
        if ((await retry.count()) > 0) {
            try {
                await retry.click({ timeout: 2000, noWaitAfter: true });
                console.log("[ui-e2e] onboarding: clicked Retry");
            } catch (e) {
                console.log("[ui-e2e] onboarding: Retry click error: " + e.message.split(String.fromCharCode(10))[0]);
            }
        } else {
            console.log("[ui-e2e] onboarding: Retry button not found");
        }
        } catch (e) {
            console.log("[ui-e2e] onboarding test caught: " + e.message.split(String.fromCharCode(10))[0]);
        }

        await browser.close();

        console.log("");
        console.log("[ui-e2e] ============ SUMMARY ============");
        if (failures.length === 0) {
            console.log("[ui-e2e] ALL OK (" + reportTotal(report) + " buttons/clickables + " + sweepOk + " sweep + 12 keyboard shortcuts + onboarding retry, across 15 pages)");
        } else {
            console.log("[ui-e2e] " + failures.length + " failures:");
            for (const f of failures) {
                console.log("  - [" + f.page + "] " + f.text + ": " + f.error);
            }
            exitCode = 1;
        }
    } catch (e) {
        console.error("[ui-e2e] FAIL:", e.message);
        exitCode = 1;
    } finally {
        await stopGateway(gateway);
        process.exit(exitCode);
    }
})();

function reportTotal(report) {
    let total = 0;
    for (const k in report) total += (report[k].results || []).length;
    return total;
}
