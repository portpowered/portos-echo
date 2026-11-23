import { test } from "node:test";
import assert from "node:assert/strict";
import { emit, pageFor } from "../../browser.mjs";
import { frame } from "../../fixtures.mjs";
import { snapshot } from "../page.mjs";

const query =
    "?events=%2Fevents%3Ftoken%3Dsynthetic-screen-pair&actions=%2Factions%3Ftoken%3Dsynthetic-screen-pair";

async function wifiPage(t, status, scan, openSettings = true) {
    const page = await pageFor(t, query, true, async (page) => {
        await page.evaluate(
            ({ status, scan, snapshot }) => {
                const meta = document.createElement("meta");
                meta.name = "echo-wifi";
                meta.content = "v1";
                document.head.append(meta);
                window.responseBody = snapshot;
                window.postStatus = 200;
                window.wifiCalls = [];
                window.wifiResults = { status, scan };
                const fetch = window.fetch;
                window.fetch = (url, options) => {
                    if (new URL(url).pathname !== "/_screen/wifi")
                        return fetch(url, options);
                    const frame = JSON.parse(options.body);
                    window.wifiCalls.push({
                        url: url.href,
                        method: options.method,
                        frame,
                    });
                    const op = frame.data.op;
                    if (op === "join" && window.deferWifiJoin)
                        return window.deferWifiJoin.promise;
                    return Promise.resolve({
                        ok: true,
                        json: async () => ({
                            type: "wifi-result",
                            data: { version: 1, op, ...window.wifiResults[op] },
                        }),
                    });
                };
            },
            { status, scan, snapshot },
        );
    });
    if (openSettings) {
        await page
            .getByRole("button", { name: "Settings", exact: true })
            .click();
        await page.locator('.settings-fields[aria-busy="false"]').waitFor();
    }
    return page;
}

test("S01–S03 Wi-Fi shows current status, ordered signal, empty scans and retry", async (t) => {
    const page = await wifiPage(
        t,
        { ok: true, ssid: "Fixture A", reachable: true },
        {
            ok: true,
            networks: [
                {
                    ssid: "Fixture B",
                    signalDbm: -42,
                    secured: true,
                    networkId: null,
                },
                {
                    ssid: "Fixture A",
                    signalDbm: -64,
                    secured: true,
                    networkId: 7,
                },
            ],
        },
    );
    await page.keyboard.press("Tab");
    await page.keyboard.press("Enter");
    await page.getByText("Current network: Fixture A (reachable)").waitFor();
    const rows = page.getByRole("list", { name: "Available networks" });
    await rows.getByText("Fixture B (-42 dBm)").waitFor();
    assert.deepEqual(await rows.locator("li > span").allTextContents(), [
        "Fixture B (-42 dBm)",
        "Fixture A (-64 dBm)",
    ]);
    await page.evaluate(() => {
        window.wifiResults.status = { ok: true, ssid: null, reachable: false };
        window.wifiResults.scan = { ok: true, networks: [] };
    });
    const retry = page.getByRole("button", { name: "Scan again" });
    await retry.click();
    await page.getByText("No current network (unreachable)").waitFor();
    await page.getByText("No networks found.").waitFor();
    assert.equal(await rows.getByRole("listitem").count(), 0);
    await page.evaluate(() => {
        window.wifiResults.scan = { ok: false, error: "wifi-unavailable" };
    });
    await retry.click();
    await page.getByText("Wi-Fi is unavailable. Try again.").waitFor();
    assert.equal(await retry.isEnabled(), true);
    assert.deepEqual(
        await page.evaluate(() => window.wifiCalls.slice(1, 3)),
        ["status", "scan"].map((op) => ({
            url: "http://127.0.0.1:4173/_screen/wifi?token=synthetic-screen-pair",
            method: "POST",
            frame: { type: "wifi-request", data: { version: 1, op } },
        })),
    );
});

test("S06–S10 Settings reports join, rollback and selected forget results", async (t) => {
    const page = await wifiPage(
        t,
        { ok: true, ssid: "Fixture A", reachable: true },
        {
            ok: true,
            networks: [
                {
                    ssid: "Fixture B",
                    signalDbm: -42,
                    secured: true,
                    networkId: null,
                },
                {
                    ssid: "Fixture A",
                    signalDbm: -64,
                    secured: true,
                    networkId: 7,
                },
                {
                    ssid: "Fixture C",
                    signalDbm: -80,
                    secured: false,
                    networkId: 12,
                },
            ],
        },
    );
    await page.getByText("Wi-Fi", { exact: true }).click();
    const list = page.getByRole("list", { name: "Available networks" });
    const select = list.getByRole("button", { name: "Join Fixture B" });
    await select.focus();
    await page.keyboard.press("Enter");
    const field = page.getByLabel("Password for Fixture B");
    assert.equal(
        await field.evaluate((element) => document.activeElement === element),
        true,
    );
    assert.equal(await field.getAttribute("type"), "password");
    await page.evaluate(() => {
        window.wifiResults.join = {
            ok: false,
            error: "wifi-join-failed",
            ssid: "Fixture A",
            reachable: true,
        };
    });
    await field.fill(["incorrect", 42].join("-"));
    await page.keyboard.press("Enter");
    await page
        .getByText("Could not join Wi-Fi. The previous network was kept.")
        .waitFor();
    await page.getByText("Current network: Fixture A (reachable)").waitFor();
    assert.equal(await field.inputValue(), "");
    await page.evaluate(() => {
        window.wifiResults.join = {
            ok: false,
            error: "wifi-rollback-failed",
            ssid: null,
            reachable: false,
        };
    });
    await field.fill(["incorrect", 42].join("-"));
    await field.press("Enter");
    await page
        .getByText(
            "Could not restore the previous network. Scan again or use USB recovery.",
        )
        .waitFor();
    await page.getByText("No current network (unreachable)").waitFor();
    await page.evaluate(() => {
        window.wifiResults.join = {
            ok: true,
            ssid: "Fixture B",
            reachable: true,
        };
    });
    await field.fill(["join", "Fixture B"].join("-"));
    await field.press("Enter");
    await page.getByText("Connected to Fixture B").waitFor();
    await page.getByText("Current network: Fixture B (reachable)").waitFor();
    await page.evaluate(() => {
        window.wifiResults.forget = {
            ok: true,
            ssid: "Fixture B",
            reachable: true,
        };
        window.wifiResults.scan.networks[1].networkId = null;
    });
    await list.getByRole("button", { name: "Forget Fixture A" }).press("Enter");
    await page.getByText("Network forgotten.").waitFor();
    await list.getByRole("button", { name: "Forget Fixture C" }).waitFor();
    assert.equal(
        await list.getByRole("button", { name: "Forget Fixture A" }).count(),
        0,
    );
    assert.equal(
        await page.evaluate(
            () =>
                window.wifiCalls.find((call) => call.frame.data.op === "forget")
                    .frame.data.networkId,
        ),
        7,
    );
});

test("S05 a legacy endpoint keeps Settings usable without a Wi-Fi scan", async (t) => {
    const page = await wifiPage(t, { ok: false, error: "wifi-unsupported" });
    assert.equal(
        await page.getByLabel("Daily request cap").inputValue(),
        "9223372036854775807",
    );
    assert.equal(
        await page.getByText("Wi-Fi", { exact: true }).isVisible(),
        false,
    );
    assert.deepEqual(
        await page.evaluate(() =>
            window.wifiCalls.map((call) => call.frame.data.op),
        ),
        ["status", "status"],
    );
});

test("the offline status poll does not compete with a pending join", async (t) => {
    const page = await wifiPage(
        t,
        { ok: true, ssid: "Fixture A", reachable: true },
        {
            ok: true,
            networks: [
                {
                    ssid: "Fixture B",
                    signalDbm: -42,
                    secured: false,
                    networkId: null,
                },
            ],
        },
    );
    await page.getByText("Wi-Fi", { exact: true }).click();
    await page.getByRole("button", { name: "Join Fixture B" }).click();
    await page.evaluate(() => {
        window.deferWifiJoin = Promise.withResolvers();
        window.wifiCalls.length = 0;
    });
    await page.getByRole("button", { name: "Join Fixture B" }).first().click();
    await page.clock.runFor(1000);
    const calls = () =>
        page.evaluate(() => window.wifiCalls.map((call) => call.frame.data.op));
    assert.deepEqual(await calls(), ["join"]);
    await page.evaluate(() =>
        window.deferWifiJoin.resolve({
            ok: true,
            json: async () => ({
                type: "wifi-result",
                data: {
                    version: 1,
                    op: "join",
                    ok: true,
                    ssid: "Fixture B",
                    reachable: true,
                },
            }),
        }),
    );
    await page.getByText("Connected to Fixture B").waitFor();
    await page.getByRole("button", { name: "Join Fixture B" }).waitFor();
    assert.deepEqual(await calls(), ["join", "status", "scan"]);
});

test("Wi-Fi opens at 30 seconds of confirmed loss and stays dismissed until recovery", async (t) => {
    const page = await wifiPage(
        t,
        { ok: true, ssid: null, reachable: false },
        { ok: true, networks: [] },
        false,
    );
    await emit(page, frame(1));
    const home = page.getByRole("region", { name: "Screen presentation" });
    const wifi = page.getByText("Wi-Fi", { exact: true });
    const disconnected = page.getByRole("status").filter({
        hasText: "No current network (unreachable)",
    });
    await page.clock.runFor(29000);
    assert.equal(await home.isVisible(), true);
    assert.equal(await wifi.isVisible(), false);
    await page.clock.runFor(1000);
    await disconnected.waitFor();
    await page.locator('.settings-fields[aria-busy="false"]').waitFor();
    assert.equal(await home.isVisible(), false);
    assert.equal(
        await wifi.evaluate((node) => node === document.activeElement),
        true,
    );
    console.log("Wi-Fi prompt: t=29s Home, t=30s Settings with Wi-Fi focused");

    await page.keyboard.press("Escape");
    const gear = page.getByRole("button", { name: "Settings", exact: true });
    assert.equal(
        await gear.evaluate((node) => node === document.activeElement),
        true,
    );
    await page.clock.runFor(31000);
    assert.equal(await home.isVisible(), true);

    const status = (value) =>
        page.evaluate((next) => (window.wifiResults.status = next), value);
    await status({ ok: false, error: "wifi-unavailable" });
    await page.clock.runFor(31000);
    await status({ ok: true, ssid: null, reachable: false });
    await page.clock.runFor(31000);
    assert.equal(await home.isVisible(), true);

    await status({ ok: true, ssid: "Fixture A", reachable: true });
    await page.clock.runFor(1000);
    await status({ ok: true, ssid: "Fixture A", reachable: false });
    await page.clock.runFor(30000);
    assert.equal(await home.isVisible(), true);
    await status({ ok: false, error: "wifi-unsupported" });
    await page.clock.runFor(31000);
    assert.equal(await home.isVisible(), true);
    await status({ ok: true, ssid: "Fixture A", reachable: false });
    await page.clock.runFor(30000);
    assert.equal(await home.isVisible(), true);
    await page.clock.runFor(1000);
    await page
        .getByRole("status")
        .filter({ hasText: "Current network: Fixture A (unreachable)" })
        .waitFor();
    assert.equal(
        await wifi.evaluate((node) => node === document.activeElement),
        true,
    );
});
