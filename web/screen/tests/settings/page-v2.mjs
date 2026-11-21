import { test } from "node:test";
import assert from "node:assert/strict";
import { verifyMuseSettings } from "./muse.mjs";
import { calls as allCalls, pageFor } from "../browser.mjs";
import { response, snapshot } from "./page.mjs";
import { snapshot as pixelSnapshot } from "../support/snapshot.mjs";

const query =
    "?events=%2Fevents%3Ftoken%3Dsynthetic-screen-pair&actions=%2Factions%3Ftoken%3Dsynthetic-screen-pair";
const scopes = [
    "endpoint:read",
    "endpoint:write",
    "message:send",
    "group:read",
    "group:manage",
    "subscription:read",
    "subscription:write",
];
const tools = [
    "control",
    "endpoint_query",
    "endpoint_show",
    "endpoint_manage",
    "group_query",
    "group_manage",
    "guide",
    "subscription_list",
    "subscription_inspect",
    "subscription_cancel",
    "subscription_label_edit",
];

export function v2Snapshot() {
    const { link_status: linkStatus, ...base } = snapshot;
    return {
        ...base,
        version: 2,
        settings: {
            ...base.settings,
            codex: {
                voice: "marin",
                web_search: "cached",
                delegate_model: "synthetic-runner",
                delegate_effort: "low",
                delegate_fast: false,
            },
            agent: {
                daily_request_cap: base.settings.agent.daily_request_cap,
            },
            home: { agent_tools: ["notes", "prompts"] },
        },
        portos: {
            link_status: linkStatus,
            operation_state: "idle",
            link_waiting_for_restart: false,
            scope_choices: scopes,
            tool_choices: tools,
            server: {
                name: "portos",
                url: "https://127.0.0.1/mcp",
                requested_scopes: ["endpoint:read", "message:send"],
                granted_scopes: [
                    "endpoint:read",
                    "message:send",
                    "endpoint:write",
                ],
                allow_tools: ["endpoint_query", "control"],
            },
        },
    };
}

async function calls(page) {
    return (await allCalls(page)).filter(
        ({ url }) => new URL(url).pathname !== "/settings/v2/openai",
    );
}

export async function open(
    t,
    settingsV2Body,
    settingsV2Status = 200,
    options = {},
) {
    const page = await pageFor(t, query, true, undefined, {
        viewport: { width: 960, height: 480 },
        hasTouch: true,
        ...options.context,
    });
    await response(page, snapshot);
    await page.evaluate(
        ({ settingsV2Body, settingsV2Status, openAIAccount }) => {
            window.settingsV2Body = settingsV2Body;
            window.settingsV2Status = settingsV2Status;
            window.openAIAccount = openAIAccount;
        },
        { settingsV2Body, settingsV2Status, openAIAccount: options.openAI },
    );
    await page.getByRole("button", { name: "Settings", exact: true }).tap();
    return page;
}

const section = (page, name) =>
    page.getByText(name, { exact: true }).and(page.locator("summary"));

test("paired Muse settings support keyboard save, test and clear without echoing private inputs", async (t) => {
    const page = await open(t, {
        ...v2Snapshot(),
        muse: { configured: false, reachability: "unchecked" },
    });
    await verifyMuseSettings(page, calls);
});

test("settings V2 displays configured options and submits only changed requested scopes", async (t) => {
    const page = await open(t, v2Snapshot());
    assert.equal(await page.getByRole("status").textContent(), "");
    const back = page.getByRole("button", { name: "Back", exact: true });
    const openai = section(page, "OpenAI");
    const portos = section(page, "Port OS permissions");
    const unlink = page.getByRole("button", {
        name: "Remove Port OS link",
        exact: true,
    });
    for (const control of [back, unlink]) {
        const { y, height } = await control.boundingBox();
        assert.ok(height >= 64 && y + height <= 480);
    }
    await pixelSnapshot(page, "settings-v2-960x480");
    await back.press("Tab");
    assert.equal(
        await unlink.evaluate((node) => node === document.activeElement),
        true,
    );
    await unlink.press("Tab");
    await page.getByRole("button", { name: "Load screen packs" }).press("Tab");
    assert.equal(
        await openai.evaluate((node) => node === document.activeElement),
        true,
    );
    await openai.press("Enter");
    for (const [label, value] of [
        ["Live voice", "marin"],
        ["Web search", "cached"],
        ["Delegated model", "synthetic-runner"],
        ["Delegated reasoning effort", "low"],
        ["Follow-up window (seconds)", "15"],
        ["Daily request cap", "9223372036854775807"],
        ["voice.md", "Voice instructions"],
        ["runner.md", "Runner instructions"],
    ]) {
        assert.equal(
            await page.getByLabel(label, { exact: true }).inputValue(),
            value,
        );
    }
    for (const label of ["Follow-up listening", "notes", "prompts"])
        assert.equal(
            await page.getByLabel(label, { exact: true }).isChecked(),
            true,
        );
    await openai.press("Enter");
    await portos.tap();
    await pixelSnapshot(page, "settings-v2-portos-960x480");
    assert.equal(
        await page.getByLabel("endpoint_query", { exact: true }).isChecked(),
        true,
    );
    assert.equal(
        await page.getByText("https://127.0.0.1/mcp", { exact: true }).count(),
        1,
    );
    assert.equal(
        await page
            .getByText(
                "Granted scopes: endpoint:read, message:send, endpoint:write",
                { exact: true },
            )
            .count(),
        1,
    );
    for (const [label, choices] of [
        ["Requested scopes", scopes],
        ["Allowed tools", tools],
    ]) {
        assert.deepEqual(
            await page
                .getByRole("group", { name: label })
                .locator("label")
                .allTextContents(),
            choices,
        );
    }
    await page.getByText("message:send", { exact: true }).tap();
    assert.equal(
        await page.getByLabel("message:send", { exact: true }).isChecked(),
        false,
    );
    await response(page, { version: 2, restart_pending: true });
    await page.getByRole("button", { name: "Save Port OS permissions" }).tap();
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        "Saved. The bridge will restart after the current reply finishes.",
    );
    assert.deepEqual(
        (await calls(page)).map(({ method, body }) => ({ method, body })),
        [
            { method: "GET", body: undefined },
            {
                method: "PATCH",
                body: '{"version":2,"settings":{"portos":{"scopes":["endpoint:read"]}}}',
            },
        ],
    );
    // Back stays the corner control (--corner-* tokens) while saving.
    assert.deepEqual(await back.boundingBox(), {
        x: 864,
        y: 12,
        width: 72,
        height: 72,
    });
    assert.equal(
        await page
            .locator(".settings-fields")
            .evaluate((node) => node.scrollHeight > node.clientHeight),
        true,
    );
    await back.tap();
    assert.equal(
        await page
            .getByRole("button", { name: "Settings", exact: true })
            .evaluate((node) => node === document.activeElement),
        true,
    );
});

test("settings V2 observes replacement without resending a successful save", async (t) => {
    const page = await open(t, v2Snapshot());
    await section(page, "OpenAI").tap();
    const search = page.getByLabel("Web search", { exact: true });
    for (const label of ["Tool calls per task", "Task deadline (seconds)"])
        assert.equal(await page.getByLabel(label).count(), 0);
    const cap = page.getByLabel("Daily request cap", { exact: true });
    await cap.focus();
    await cap.press("Tab");
    await page.keyboard.press("Tab");
    assert.equal(
        await search.evaluate((node) => node === document.activeElement),
        true,
    );
    const fast = page.getByRole("checkbox", { name: "Delegated fast mode" });
    assert.equal(await fast.isChecked(), false);
    const save = page.getByRole("button", {
        name: "Save settings",
        exact: true,
    });
    await save.tap();
    assert.equal(
        await page.getByRole("status").textContent(),
        "No settings changed",
    );
    await page.getByLabel("Delegated reasoning effort").press("Tab");
    assert.equal(
        await fast.evaluate((node) => node === document.activeElement),
        true,
    );
    const box = await fast.boundingBox();
    assert.ok(box.width >= 64 && box.height >= 64 && box.y + box.height <= 480);
    await fast.press("Space");
    assert.equal(await fast.isChecked(), true);
    await page.screenshot({
        path: "test-results/screenshots/settings-fast-on-960x480.png",
    });
    await response(page, { version: 2, restart_pending: true });
    await save.press("Enter");
    const status = page.getByRole("status");
    const saved =
        "Saved. The bridge will restart after the current reply finishes.";
    await page.evaluate(() => {
        window.settingsV2Body.restart_pending = true;
        window.settingsV2Body.settings.codex.delegate_fast = true;
    });
    await page.clock.runFor(500);
    assert.equal(await status.textContent(), saved);
    await page.evaluate(() => {
        window.settingsV2Status = 503;
    });
    await page.clock.runFor(500);
    assert.equal(await status.textContent(), saved);
    await page.evaluate(() => {
        window.settingsV2Status = 200;
        window.settingsV2Body.restart_pending = false;
    });
    await page.clock.runFor(500);
    assert.equal(
        await status.textContent(),
        "Bridge restarted. Saved settings are active.",
    );
    // A result clears itself 4 s after it was written; a newer one restarts it.
    await page.clock.runFor(3999);
    assert.equal(
        await status.textContent(),
        "Bridge restarted. Saved settings are active.",
    );
    await page.clock.runFor(1);
    assert.equal(await status.textContent(), "");
    assert.equal(await fast.isChecked(), true);
    assert.deepEqual(
        JSON.parse(
            (await calls(page)).find(({ method }) => method === "PATCH").body,
        ),
        { version: 2, settings: { codex: { delegate_fast: true } } },
    );
    assert.deepEqual(
        (await calls(page)).map(({ method }) => method),
        ["GET", "PATCH", "GET", "GET", "GET"],
    );
});

test("settings V2 keeps an explicit fast-mode off edit after a failed save", async (t) => {
    const data = v2Snapshot();
    data.settings.codex.delegate_fast = true;
    const page = await open(t, data, 200, {
        context: { viewport: { width: 960, height: 600 } },
    });
    await section(page, "OpenAI").tap();
    const fast = page.getByRole("checkbox", { name: "Delegated fast mode" });
    assert.equal(await fast.isChecked(), true);
    await fast.press("Space");
    assert.equal(await fast.isChecked(), false);
    const box = await fast.boundingBox();
    assert.ok(box.width >= 64 && box.height >= 64 && box.y + box.height <= 600);
    await page.screenshot({
        path: "test-results/screenshots/settings-fast-off-960x600.png",
    });
    const save = page.getByRole("button", {
        name: "Save settings",
        exact: true,
    });
    const status = page.getByRole("status");
    const error = "Could not save file; previous file retained";
    await response(page, { version: 2, message: error }, 500);
    await save.tap();
    assert.equal(await status.textContent(), error);
    assert.equal(await fast.isChecked(), false);
    await response(page, { version: 2, restart_pending: true });
    await save.tap();
    assert.equal(
        await status.textContent(),
        "Saved. The bridge will restart after the current reply finishes.",
    );
    assert.deepEqual(
        (await calls(page))
            .filter(({ method }) => method === "PATCH")
            .map(({ body }) => JSON.parse(body)),
        Array.from({ length: 2 }, () => ({
            version: 2,
            settings: { codex: { delegate_fast: false } },
        })),
    );
});

test("settings V2 refuses invalid and unauthorized replies without falling back", async (t) => {
    const missingFast = v2Snapshot();
    delete missingFast.settings.codex.delegate_fast;
    const invalidFast = v2Snapshot();
    invalidFast.settings.codex.delegate_fast = "false";
    for (const [status, body, message] of [
        [200, snapshot, "Settings response could not be read"],
        [200, missingFast, "Settings response could not be read"],
        [200, invalidFast, "Settings response could not be read"],
        [
            200,
            { ...v2Snapshot(), extra: true },
            "Settings response could not be read",
        ],
        [401, undefined, "Pairing is unavailable. Reopen the paired screen."],
    ]) {
        const page = await open(t, body, status);
        assert.equal(
            await page.locator(".settings-panel [role=status]").textContent(),
            message,
        );
        assert.deepEqual(
            (await calls(page)).map(({ url, method }) => [
                new URL(url).pathname,
                method,
            ]),
            [["/settings/v2", "GET"]],
        );
        assert.equal(
            await page
                .getByRole("button", { name: "Save settings", exact: true })
                .count(),
            0,
        );
    }
});

test("settings V2 leaves a failed replacement pending when Settings reopens", async (t) => {
    const data = v2Snapshot();
    data.restart_pending = true;
    data.settings.codex.delegate_fast = true;
    data.warnings = ["Could not restart bridge; saved settings remain pending"];
    const page = await open(t, data);
    assert.equal(
        await page.getByRole("status").textContent(),
        data.warnings[0],
    );
    // The restart warning is information the owner needs: it never fades.
    await page.clock.runFor(10000);
    assert.equal(
        await page.getByRole("status").textContent(),
        data.warnings[0],
    );
    assert.deepEqual(
        (await calls(page)).map(({ method }) => method),
        ["GET"],
    );
});

export async function portosStatus(page, updates) {
    await page.evaluate((updates) => {
        Object.assign(window.settingsV2Body.portos, updates);
    }, updates);
}

test("settings V2 shows Port OS approval and confirms unlink exactly once", async (t) => {
    const data = v2Snapshot();
    data.portos.link_status = "link_required";
    data.portos.server.granted_scopes = [];
    const page = await open(t, data);
    assert.equal(await page.getByRole("status").textContent(), "");
    await section(page, "OpenAI").tap();
    const model = page.getByLabel("Delegated model");
    await model.fill("unsaved-local-model");
    await response(page, { version: 2, operation: "link" }, 202);
    await page
        .getByRole("button", { name: "Link Port OS", exact: true })
        .click();
    await portosStatus(page, {
        operation_state: "linking",
        challenge: {
            verification_uri: "https://127.0.0.1/approve",
            user_code: "TEST-CODE",
            expires_at_unix: 2000000000,
        },
    });
    await page.clock.runFor(500);
    const code = page.getByText(/^Code: TEST-CODE · Expires at /);
    await code.waitFor();
    await page
        .getByText("On a phone or PC, open https://127.0.0.1/approve")
        .waitFor();
    assert.equal(await model.inputValue(), "unsaved-local-model");
    await page.evaluate(() => {
        const portos = window.settingsV2Body.portos;
        portos.operation_state = "idle";
        portos.link_status = "linked";
        portos.server.granted_scopes = ["endpoint:read"];
        delete portos.challenge;
    });
    await page.clock.runFor(500);
    await code.waitFor({ state: "hidden" });
    const unlink = page.getByRole("button", {
        name: "Remove Port OS link",
    });
    await unlink.click();
    const cancel = page.getByRole("button", { name: "Cancel", exact: true });
    assert.equal(
        await cancel.evaluate((node) => node === document.activeElement),
        true,
    );
    await cancel.press("Escape");
    assert.equal(
        await unlink.evaluate((node) => node === document.activeElement),
        true,
    );
    const mutations = async () =>
        (await calls(page))
            .filter(({ method }) => method === "POST")
            .map(({ url, body }) => [new URL(url).pathname, JSON.parse(body)]);
    assert.deepEqual(await mutations(), [
        ["/settings/v2/portos", { version: 2, operation: "link" }],
    ]);
    await unlink.click();
    await response(page, { version: 2, operation: "unlink" }, 202);
    await page
        .getByRole("button", { name: "Remove link", exact: true })
        .click();
    await portosStatus(page, { link_status: "link_required" });
    await page.clock.runFor(500);
    await page.getByText("Link required", { exact: true }).waitFor();
    assert.deepEqual(await mutations(), [
        ["/settings/v2/portos", { version: 2, operation: "link" }],
        [
            "/settings/v2/portos",
            { version: 2, operation: "unlink", confirm: true },
        ],
    ]);
});
