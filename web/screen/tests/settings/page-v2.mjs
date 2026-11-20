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
