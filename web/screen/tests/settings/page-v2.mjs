import { test } from "node:test";
import assert from "node:assert/strict";
import { calls, pageFor } from "../browser.mjs";
import { response, snapshot } from "./page.mjs";

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

function v2Snapshot() {
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
            },
            agent: {
                ...base.settings.agent,
                max_tool_calls: 6,
                task_deadline_seconds: 45,
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

async function open(t, settingsV2Body, settingsV2Status = 200) {
    const page = await pageFor(t, query, true);
    page.setDefaultTimeout(5000);
    await page.setViewportSize({ width: 960, height: 480 });
    await response(page, snapshot);
    await page.evaluate(
        ({ settingsV2Body, settingsV2Status }) => {
            window.settingsV2Body = settingsV2Body;
            window.settingsV2Status = settingsV2Status;
        },
        { settingsV2Body, settingsV2Status },
    );
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    return page;
}

test("settings V2 displays configured options and submits only changed requested scopes", async (t) => {
    const page = await open(t, v2Snapshot());
    assert.equal(
        await page.getByRole("status").textContent(),
        "Settings loaded",
    );
    for (const name of ["OpenAI", "Port OS"])
        assert.equal(
            await page.getByRole("heading", { name, exact: true }).count(),
            1,
        );
    for (const [label, value] of [
        ["Live voice", "marin"],
        ["Web search", "cached"],
        ["Delegated model", "synthetic-runner"],
        ["Delegated reasoning effort", "low"],
        ["Tool calls per task", "6"],
        ["Task deadline (seconds)", "45"],
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
    for (const label of [
        "Follow-up listening",
        "notes",
        "prompts",
        "endpoint_query",
    ])
        assert.equal(
            await page.getByLabel(label, { exact: true }).isChecked(),
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
    await page.getByLabel("message:send", { exact: true }).uncheck();
    await response(page, { version: 2, restart_pending: true });
    await page
        .getByRole("button", { name: "Save Port OS permissions" })
        .click();
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
});

test("settings V2 refuses invalid and unauthorized replies without falling back", async (t) => {
    for (const [status, body, message] of [
        [200, snapshot, "Settings response could not be read"],
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

async function portosStatus(page, updates) {
    await page.evaluate((updates) => {
        Object.assign(window.settingsV2Body.portos, updates);
    }, updates);
}

test("settings V2 shows Port OS approval and confirms unlink exactly once", async (t) => {
    const data = v2Snapshot();
    data.portos.link_status = "link_required";
    data.portos.server.granted_scopes = [];
    const page = await open(t, data);
    assert.equal(
        await page.getByRole("status").textContent(),
        "Settings loaded",
    );
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
    const unlink = page.getByRole("button", { name: "Unlink Port OS" });
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
    await page.getByRole("button", { name: "Confirm unlink" }).click();
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
