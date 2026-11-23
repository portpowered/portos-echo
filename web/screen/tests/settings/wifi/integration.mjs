import assert from "node:assert/strict";
import { startScreenFixture } from "../../support/server-fixture.mjs";

export async function verifyWifiPrompt(t, browser) {
    const fixture = await startScreenFixture(true);
    const context = await browser.newContext({ serviceWorkers: "block" });
    const rescanReady = Promise.withResolvers();
    const releaseRescan = Promise.withResolvers();
    t.after(async () => {
        releaseRescan.resolve();
        await context.close();
        await fixture.stop();
    });
    context.setDefaultTimeout(5000);
    const credential = ["incorrect", 42].join("-");
    let holdRescan = false;
    let joins = 0;
    let submissionPreserved = false;
    await context.route("**/*", async (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin !== fixture.origin) return route.abort();
        if (url.pathname === "/_screen/wifi") {
            const { data } = request.postDataJSON();
            if (data.op === "join") {
                joins++;
                submissionPreserved =
                    data.ssid === "Fixture B" && data.password === credential;
            }
            if (data.op === "scan" && holdRescan) {
                holdRescan = false;
                const response = await route.fetch({ maxRedirects: 0 });
                rescanReady.resolve();
                await releaseRescan.promise;
                return route.fulfill({ response });
            }
        }
        return route.continue();
    });
    // The real HTTP response is read before advancing the clock to the next poll.
    let statusReads = 0;
    let statusRead = Promise.withResolvers();
    await context.exposeBinding("recordWifiStatus", () => {
        statusReads++;
        statusRead.resolve();
        statusRead = Promise.withResolvers();
    });
    await context.addInitScript(() => {
        const json = Response.prototype.json;
        Response.prototype.json = async function () {
            const value = await json.call(this);
            if (value?.type === "wifi-result" && value.data.op === "status")
                queueMicrotask(() => void window.recordWifiStatus());
            return value;
        };
    });
    const page = await context.newPage();
    await page.clock.install({ time: 0 });
    await page.clock.pauseAt(0);
    fixture.command({ state: { state: "ready", message: "Ready" } });
    await page.goto(fixture.pairedUrl);
    const home = page.getByRole("region", { name: "Screen presentation" });
    await page.locator('[data-ui-state="ready"]').waitFor();
    const wifi = page.getByText("Wi-Fi", { exact: true });
    async function waitForStatus(expected) {
        while (statusReads < expected) await statusRead.promise;
    }
    await waitForStatus(1);
    for (let second = 1; second <= 29; second++) {
        await page.clock.runFor(1000);
        await waitForStatus(second + 1);
    }
    assert.equal(await home.isVisible(), true);
    assert.equal(await wifi.isVisible(), false);
    await page.clock.runFor(1000);
    await page.getByText("No current network (unreachable)").waitFor();
    await page.locator('.settings-fields[aria-busy="false"]').waitFor();
    assert.equal(await home.isVisible(), false);
    assert.equal(
        await wifi.evaluate((node) => document.activeElement === node),
        true,
    );

    const join = page.getByRole("button", { name: "Join Fixture B" });
    await join.waitFor();
    await page.keyboard.press("Tab");
    assert.equal(
        await join.evaluate((node) => document.activeElement === node),
        true,
    );
    await page.keyboard.press("Enter");
    const field = page.getByLabel("Password for Fixture B");
    assert.equal(
        await field.evaluate((node) => document.activeElement === node),
        true,
    );
    await field.fill(credential);
    holdRescan = true;
    const retry = page.getByRole("button", { name: "Scan again" });
    await retry.click();
    await rescanReady.promise;
    await field.press("Enter");
    await page.getByText("Joining Wi-Fi…", { exact: true }).waitFor();
    assert.equal(await field.isDisabled(), true);
    assert.equal(joins, 0);
    releaseRescan.resolve();
    await page
        .getByRole("status")
        .filter({
            hasText: "Could not join Wi-Fi. The previous network was kept.",
        })
        .waitFor();
    assert.equal(joins, 1);
    assert.equal(submissionPreserved, true);
    assert.equal((await field.inputValue()) === "", true);
    await page.keyboard.press("Escape");
    const gear = page.getByRole("button", { name: "Settings", exact: true });
    assert.equal(
        await gear.evaluate((node) => document.activeElement === node),
        true,
    );
    assert.equal(await home.isVisible(), true);
    console.log(
        "Paired Wi-Fi on loopback: Home at 29s, focused Wi-Fi at 30s, one intact keyboard join after a held rescan, rejection shown, password cleared, Escape returns focus to Settings",
    );
}
