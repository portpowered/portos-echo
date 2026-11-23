import { test } from "node:test";
import assert from "node:assert/strict";
import { pageFor } from "../../browser.mjs";
import { snapshot } from "../page.mjs";

const query =
    "?events=%2Fevents%3Ftoken%3Dsynthetic-screen-pair&actions=%2Factions%3Ftoken%3Dsynthetic-screen-pair";

async function wifiPage(t, status, scan) {
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
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.locator('.settings-fields[aria-busy="false"]').waitFor();
    return page;
}

test("S01–S03 Wi-Fi shows current status, ordered signal, empty scans and retry", async (t) => {
    const page = await wifiPage(
        t,
        { ok: true, ssid: "Fixture A", reachable: true },
        {
            ok: true,
            networks: [
                { ssid: "Fixture B", signalDbm: -42 },
                { ssid: "Fixture A", signalDbm: -64 },
            ],
        },
    );
    await page.keyboard.press("Tab");
    await page.keyboard.press("Enter");
    await page.getByText("Current network: Fixture A (reachable)").waitFor();
    const rows = page.getByRole("list", { name: "Available networks" });
    await rows.getByText("Fixture B (-42 dBm)").waitFor();
    assert.deepEqual(await rows.getByRole("listitem").allTextContents(), [
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
        await page.evaluate(() => window.wifiCalls.slice(0, 2)),
        ["status", "scan"].map((op) => ({
            url: "http://127.0.0.1:4173/_screen/wifi?token=synthetic-screen-pair",
            method: "POST",
            frame: { type: "wifi-request", data: { version: 1, op } },
        })),
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
        ["status"],
    );
});
