import { test } from "node:test";
import assert from "node:assert/strict";
import { calls as allCalls } from "../browser.mjs";
import { response } from "./page.mjs";
import { open, portosStatus, v2Snapshot } from "./page-v2.mjs";
import { snapshot as pixelSnapshot } from "../support/snapshot.mjs";

test("960×600 Settings starts with both Accounts, then Presentation", async (t) => {
    const data = v2Snapshot();
    data.portos.link_status = "link_required";
    const page = await open(t, data, 200, {
        context: { viewport: { width: 960, height: 600 } },
        openAI: {
            version: 2,
            link_status: "link_required",
            operation_state: "idle",
        },
    });
    page.setDefaultTimeout(5000);
    const sections = page.locator(".settings-fields > *");
    assert.deepEqual(
        await sections.evaluateAll((nodes) =>
            nodes.slice(0, 2).map((node) => node.getAttribute("aria-label")),
        ),
        ["Accounts", "Presentation"],
    );
    const accounts = page.getByRole("region", { name: "Accounts" });
    const openAI = accounts.getByRole("region", { name: "OpenAI (GPT Live)" });
    const portos = accounts.getByRole("region", { name: "Port OS" });
    await openAI.getByText("Not linked", { exact: true }).waitFor();
    await portos.getByText("Link required", { exact: true }).waitFor();
    const presentation = page.getByRole("button", {
        name: "Load screen packs",
    });
    const placement = await presentation.boundingBox();
    assert.ok(placement.height >= 64 && placement.y + placement.height <= 600);
    await pixelSnapshot(page, "settings-accounts-960x600");
    const setOpenAI = (updates) =>
        page.evaluate((data) => {
            window.openAIAccount = JSON.parse(
                JSON.stringify({ version: 2, ...data }),
            );
        }, updates);
    for (const [name, row, setStatus, endpoint] of [
        ["OpenAI", openAI, setOpenAI, "openai"],
        ["Port OS", portos, (updates) => portosStatus(page, updates), "portos"],
    ]) {
        await response(page, { version: 2, operation: "link" }, 202);
        const link = row.getByRole("button", {
            name: "Link " + name,
            exact: true,
        });
        const box = await link.boundingBox();
        assert.ok(box.height >= 64 && box.y + box.height <= 600);
        await setStatus({
            link_status: "link_required",
            operation_state: "linking",
            challenge: {
                verification_uri: "https://127.0.0.1/approve",
                user_code: "LOCAL-" + endpoint,
                expires_at_unix: 2000000000,
            },
        });
        await link.tap();
        await page.clock.runFor(500);
        await row
            .getByText("On a phone or PC, open https://127.0.0.1/approve")
            .waitFor();
        const code = row.getByText(
            "Code: LOCAL-" + endpoint + " · Expires at 3:33:20 AM",
            {
                exact: true,
            },
        );
        await code.waitFor();
        await setStatus({
            link_status: "linked",
            operation_state: "idle",
            challenge: undefined,
        });
        if (endpoint === "portos")
            await page.evaluate(
                () => delete window.settingsV2Body.portos.challenge,
            );
        await page.clock.runFor(500);
        await code.waitFor({ state: "hidden" });
        await row.getByText("Linked", { exact: true }).waitFor();
        await confirmAccountUnlink(page, row, name, endpoint, () =>
            setStatus({
                link_status: "link_required",
                operation_state: "idle",
            }),
        );
        await page.clock.runFor(500);
        await link.waitFor();
    }
    await verifyPresentation(page);
    console.log(
        "960x600: Accounts above Presentation, one Link and a confirmed Remove beside each title, explicit pack save",
    );
});

// Remove sits on the title's line, at the row's top right, with a 64 px target.
async function assertRemoveBesideTitle(row, remove) {
    assert.equal(await remove.textContent(), "Remove");
    const title = await row.getByRole("heading", { level: 3 }).boundingBox();
    const status = await row.getByText("Linked", { exact: true }).boundingBox();
    const button = await remove.boundingBox();
    const box = await row.boundingBox();
    assert.ok(button.height >= 64 && button.width >= 64, "touch target");
    assert.ok(
        Math.abs(title.y + title.height / 2 - (button.y + button.height / 2)) <=
            1,
        "Remove is centred on the title's line",
    );
    assert.ok(button.y + button.height <= status.y, "above the status line");
    assert.ok(button.x > title.x + title.width, "beside the title");
    assert.ok(
        Math.abs(box.x + box.width - (button.x + button.width)) <= 1,
        "right-aligned in the row",
    );
}

async function confirmAccountUnlink(page, row, name, endpoint, setUnlinked) {
    const unlink = row.getByRole("button", {
        name: "Remove " + name + " link",
        exact: true,
    });
    await assertRemoveBesideTitle(row, unlink);
    const mutations = async () =>
        (await allCalls(page))
            .filter(
                ({ method, url }) =>
                    method === "POST" &&
                    new URL(url).pathname === "/settings/v2/" + endpoint,
            )
            .map(({ body }) => JSON.parse(body));
    await unlink.tap();
    await row.getByRole("button", { name: "Cancel", exact: true }).tap();
    assert.deepEqual(await mutations(), [{ version: 2, operation: "link" }]);
    await unlink.tap();
    await response(page, { version: 2, operation: "unlink" }, 202);
    await setUnlinked();
    await row.getByText("Remove the " + name + " link?").waitFor();
    await row.getByRole("button", { name: "Remove link", exact: true }).tap();
    assert.deepEqual(await mutations(), [
        { version: 2, operation: "link" },
        { version: 2, operation: "unlink", confirm: true },
    ]);
}

async function verifyPresentation(page) {
    const section = page.getByRole("region", { name: "Presentation" });
    const catalog = {
        version: 1,
        revision: 1,
        active: "template@0.2.0",
        warning: null,
        jobs: [],
        installed: [
            { name: "layout", id: "layout", version: "1.0.0", rest_url: null },
            {
                name: "template",
                id: "template@0.2.0",
                version: "0.2.0",
                rest_url: null,
            },
        ],
    };
    await response(page, catalog);
    await section.getByRole("button", { name: "Load screen packs" }).tap();
    const choices = section.getByRole("combobox", { name: "Screen pack" });
    assert.deepEqual(await choices.locator("option").allTextContents(), [
        "Default",
        "template 0.2.0",
        "layout 1.0.0",
        "Disable screen packs",
    ]);
    // An Echo with only the pre-rename "template" pack still has a Default.
    assert.equal(
        await choices.locator('option[value="default"]').isDisabled(),
        false,
    );
    await choices.selectOption("layout");
    await page.evaluate((model) => {
        const fetch = (window.fixtureFetch = window.fetch);
        window.fetch = (url, options) => {
            window.responseBody =
                options.method === "POST"
                    ? { version: 1, restart_pending: false }
                    : { ...model, active: "layout" };
            return fetch(url, options);
        };
    }, catalog);
    await section.getByRole("button", { name: "Use screen pack" }).tap();
    assert.equal(await choices.inputValue(), "layout");
    const saves = (await allCalls(page)).filter(
        ({ url }) => new URL(url).pathname === "/screen-packs/v1/selection",
    );
    assert.deepEqual(
        saves.map(({ method, body }) => [method, JSON.parse(body)]),
        [["POST", { version: 1, screen_pack: "layout" }]],
    );
    await chooseDefault(page, section, catalog);
}

// With "default" installed beside the legacy "template", Default lists the
// default packs first and saves the name "default" for the bridge to resolve.
async function chooseDefault(page, section, catalog) {
    const installed = [
        {
            name: "default",
            id: "default@0.3.0",
            version: "0.3.0",
            rest_url: null,
        },
        ...catalog.installed,
    ];
    await page.evaluate(
        (model) => {
            window.fetch = (url, options) => {
                window.responseBody =
                    options.method === "POST"
                        ? { version: 1, restart_pending: false }
                        : model;
                return window.fixtureFetch(url, options);
            };
        },
        { ...catalog, active: "default@0.3.0", installed },
    );
    await section.getByRole("button", { name: "Refresh packs" }).tap();
    const choices = section.getByRole("combobox", { name: "Screen pack" });
    assert.deepEqual(await choices.locator("option").allTextContents(), [
        "Default",
        "default 0.3.0",
        "template 0.2.0",
        "layout 1.0.0",
        "Disable screen packs",
    ]);
    assert.equal(await choices.inputValue(), "default@0.3.0");
    await choices.selectOption({ label: "Default" });
    await section.getByRole("button", { name: "Use screen pack" }).tap();
    const saves = (await allCalls(page)).filter(
        ({ url }) => new URL(url).pathname === "/screen-packs/v1/selection",
    );
    assert.deepEqual(JSON.parse(saves.at(-1).body), {
        version: 1,
        screen_pack: "default",
    });
}

// Settings at 960x600 is one column: text starts on the title's left edge and
// every right-side control, each section's save button included, ends on
// Back's right edge.
for (const [state, portos, label, action] of [
    ["linked", "linked", "Linked", "Remove Port OS link"],
    ["unlinked", "unconfigured", "Unconfigured", "Link Port OS"],
    ["link-required", "link_required", "Link required", "Link Port OS"],
])
    test(`960�600 Settings is one column with Port OS ${state}`, async (t) => {
        const data = v2Snapshot();
        data.portos.link_status = portos;
        const page = await open(t, data, 200, {
            context: { viewport: { width: 960, height: 600 } },
            openAI: {
                version: 2,
                link_status: "linked",
                operation_state: "idle",
            },
        });
        const row = page.getByRole("region", { name: "Port OS" });
        await row.getByText(label, { exact: true }).waitFor();
        await pixelSnapshot(page, "settings-portos-" + state);
        const edges = await columnEdges(page);
        assert.deepEqual(edges.column, { left: 24, right: 936 });
        const off = (list, edge) =>
            list.filter(([, x]) => Math.abs(x - edge) > 1);
        assert.deepEqual(off(edges.lefts, 24), [], "left edges");
        assert.deepEqual(off(edges.rights, 936), [], "right edges");
        assert.equal(new Set(edges.controls.map(Math.round)).size, 1);
        const widths = new Set(edges.actions.map(([, width]) => width));
        assert.deepEqual(
            edges.actions.map(([name]) => name),
            ["Remove OpenAI link", action],
        );
        assert.equal(widths.size, 1, JSON.stringify(edges.actions));
        for (const [name, , centre] of edges.actions)
            assert.ok(Math.abs(centre) <= 1, name + " centred on its title");
        const load = await page
            .getByRole("button", { name: "Load screen packs" })
            .boundingBox();
        assert.deepEqual([load.x, load.x + load.width], [24, 936]);
    });

// With every section open, the left text edge of each text block, the left
// edge of each left-anchored block and of each row control, and the right edge
// of each right-side control.
async function columnEdges(page) {
    await page.evaluate(() => {
        for (const details of document.querySelectorAll(".settings-section"))
            details.open = true;
    });
    const fields = ".settings-fields ";
    const [[, left]] = await edges(page, ".settings-panel h1", "textLeft");
    const [[, right]] = await edges(page, ".corner-control", "right");
    const text =
        ".settings-panel :is(h1, h2, h3, p, legend, .settings-row > label)";
    const blocks =
        ":is(section, form, fieldset, .settings-row, .settings-actions, textarea, button:not(.settings-item-header > *, .settings-stepper > *, .settings-actions > *))";
    const ends =
        ":is(section, .settings-row, .settings-row > :last-child, .settings-item-header > button, .settings-actions > :last-child, select, textarea, input:not([type=checkbox], .settings-stepper > *))";
    const controls =
        ".settings-row:not(.settings-row-wide) > :last-child:not([type=checkbox])";
    return {
        column: { left, right },
        lefts: [
            ...(await edges(page, text, "textLeft")),
            ...(await edges(page, fields + "summary", "textLeft")),
            ...(await edges(page, fields + blocks, "left")),
        ],
        rights: [
            ...(await edges(page, fields + ends, "right")),
            ...(await edges(page, fields + "summary", "textRight")),
        ],
        controls: (await edges(page, fields + controls, "left")).map(
            ([, x]) => x,
        ),
        actions: await accountActions(page),
    };
}

// [name, x] for each visible node matching `selector`; `edge` is the box's
// "left" or "right", or "textLeft"/"textRight" inside border and padding.
function edges(page, selector, edge) {
    return page.evaluate(
        ([selector, edge]) =>
            [...document.querySelectorAll(selector)]
                .filter(
                    (node) =>
                        node.checkVisibility() &&
                        node.getBoundingClientRect().width > 0,
                )
                .map((node) => {
                    const box = node.getBoundingClientRect();
                    const style = getComputedStyle(node);
                    const inner = (side) =>
                        parseFloat(style["border" + side + "Width"]) +
                        parseFloat(style["padding" + side]);
                    const name =
                        node.getAttribute("aria-label") || node.textContent;
                    return [
                        node.tagName.toLowerCase() +
                            " " +
                            name.trim().slice(0, 32),
                        {
                            left: box.left,
                            right: box.right,
                            textLeft: box.left + inner("Left"),
                            textRight: box.right - inner("Right"),
                        }[edge],
                    ];
                }),
        [selector, edge],
    );
}

// Each visible account action's name, width, and how far its centre sits
// from its title's.
function accountActions(page) {
    return page.evaluate(() => {
        const centre = (node) => {
            const box = node.getBoundingClientRect();
            return box.top + box.height / 2;
        };
        return [...document.querySelectorAll(".settings-item-header > button")]
            .filter((node) => node.checkVisibility())
            .map((node) => [
                node.getAttribute("aria-label") || node.textContent,
                node.getBoundingClientRect().width,
                centre(node) - centre(node.parentElement.querySelector("h3")),
            ]);
    });
}
