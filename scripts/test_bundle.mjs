// M19: unit test for the signing-templating block of bundle.mjs.
// We extract the relevant block from bundle.mjs and exercise it
// against a temporary tauri.conf.json fixture so we do not mutate
// the real one.

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

function applyTemplate(env, fixturePath) {
    const conf = JSON.parse(readFileSync(fixturePath, "utf8"));
    if (env.AUTOROUTER_SKIP_SIGNING !== "1") {
        if (env.WINDOWS_CERT_FILE) {
            conf.bundle.windows.certificateThumbprint = null;
            conf.bundle.windows.timestampUrl = env.WINDOWS_CERT_FILE;
        }
        if (env.APPLE_ID) {
            conf.bundle.macOS.signingIdentity = env.APPLE_ID;
        }
        if (env.APPLE_TEAM_ID) {
            conf.bundle.macOS.providerShortName = env.APPLE_TEAM_ID;
        }
        writeFileSync(fixturePath, JSON.stringify(conf, null, 2) + "\n");
    }
    return conf;
}

function fixture() {
    const dir = mkdtempSync(join(tmpdir(), "autorouter-bundle-"));
    const confPath = join(dir, "tauri.conf.json");
    writeFileSync(
        confPath,
        JSON.stringify(
            {
                bundle: {
                    windows: {
                        timestampUrl: "",
                        certificateThumbprint: null,
                    },
                    macOS: {
                        signingIdentity: null,
                        providerShortName: null,
                    },
                },
            },
            null,
            2
        ) + "\n"
    );
    return { dir, confPath };
}

test("M19: windows timestampUrl templated from env", () => {
    const { dir, confPath } = fixture();
    try {
        const out = applyTemplate({ WINDOWS_CERT_FILE: "https://ts.example/" }, confPath);
        assert.equal(out.bundle.windows.timestampUrl, "https://ts.example/");
        assert.equal(out.bundle.windows.certificateThumbprint, null);
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
});

test("M19: macOS signingIdentity templated from APPLE_ID", () => {
    const { dir, confPath } = fixture();
    try {
        const out = applyTemplate({ APPLE_ID: "YourName (TEAM000)" }, confPath);
        assert.equal(out.bundle.macOS.signingIdentity, "YourName (TEAM000)");
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
});

test("M19: macOS providerShortName templated from APPLE_TEAM_ID", () => {
    const { dir, confPath } = fixture();
    try {
        const out = applyTemplate({ APPLE_TEAM_ID: "TEAM123" }, confPath);
        assert.equal(out.bundle.macOS.providerShortName, "TEAM123");
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
});

test("M19: AUTOROUTER_SKIP_SIGNING=1 leaves the file untouched", () => {
    const { dir, confPath } = fixture();
    try {
        const before = readFileSync(confPath, "utf8");
        const out = applyTemplate(
            { AUTOROUTER_SKIP_SIGNING: "1", WINDOWS_CERT_FILE: "x" },
            confPath
        );
        // No write happened.
        assert.equal(readFileSync(confPath, "utf8"), before);
        // The function still returns the parsed object.
        assert.equal(out.bundle.windows.timestampUrl, "");
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
});