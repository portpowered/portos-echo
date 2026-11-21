import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { response } from "./page.mjs";

export async function verifyMuseSettings(page, calls) {
    const summary = page
        .getByText("Muse", { exact: true })
        .and(page.locator("summary"));
    await summary.focus();
    await page.keyboard.press("Enter");
    const url = page.getByLabel("Muse bridge URL");
    const ca = page.getByLabel("Muse CA (base64 DER)");
    const credential = page.getByLabel("Muse credential");
    const save = page.getByRole("button", { name: "Save Muse" });
    const testConnection = page.getByRole("button", {
        name: "Test connection",
    });
    const clear = page.getByRole("button", { name: "Clear Muse" });
    const origin = "https://127.0.0.1:9443";
    assert.equal(await testConnection.isDisabled(), true);
    assert.equal(await credential.getAttribute("type"), "password");
    assert.equal(await credential.inputValue(), "");
    assert.equal(await ca.inputValue(), "");
    const load = (await calls(page))[0];
    assert.equal(new URL(load.url).searchParams.get("include_muse"), "true");
    assert.equal(
        new URL(load.url).searchParams.get("token"),
        "synthetic-screen-pair",
    );
    await url.fill(origin);
    await ca.fill("c3ludGhldGlj");
    await credential.fill("synthetic-muse-private-entry");
    await response(page, { version: 2, restart_pending: true });
    await credential.press("Tab");
    assert.equal(
        await save.evaluate((node) => document.activeElement === node),
        true,
    );
    await page.keyboard.press("Enter");
    await page
        .getByText("Saved. Muse changes are waiting for the bridge restart.", {
            exact: true,
        })
        .waitFor();
    assert.deepEqual(JSON.parse((await calls(page)).at(-1).body), {
        version: 2,
        settings: {
            muse: {
                url: origin,
                ca: "c3ludGhldGlj",
                credential: "synthetic-muse-private-entry",
            },
        },
    });
    assert.equal(await credential.inputValue(), "");
    assert.equal(await ca.inputValue(), "");
    await url.fill("https://127.0.0.1:9444");
    assert.equal(await testConnection.isDisabled(), true);
    await url.fill(origin);
    await response(page, { version: 2, reachable: true });
    await testConnection.focus();
    await page.keyboard.press("Enter");
    await page.getByText("Muse is reachable.", { exact: true }).waitFor();
    const probe = (await calls(page)).at(-1);
    assert.equal(new URL(probe.url).pathname, "/settings/v2/muse");
    assert.equal(probe.method, "POST");
    assert.deepEqual(JSON.parse(probe.body), {
        version: 2,
        operation: "probe",
    });
    assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth),
        960,
    );
    await mkdir("test-results/screenshots", { recursive: true });
    await page.screenshot({
        path: "test-results/screenshots/settings-muse-960x480.png",
    });
    await response(page, {
        version: 2,
        reachable: false,
        reason: "unavailable",
    });
    await testConnection.press("Enter");
    await page
        .getByText("Muse is unavailable. Check that the service is running.", {
            exact: true,
        })
        .waitFor();
    await response(page, { version: 2, restart_pending: true });
    await clear.press("Enter");
    await page.getByText("Muse is unconfigured.", { exact: true }).waitFor();
    assert.deepEqual(JSON.parse((await calls(page)).at(-1).body), {
        version: 2,
        settings: { muse: { clear: true } },
    });
    assert.equal(await testConnection.isDisabled(), true);
    assert.equal(await url.inputValue(), "");
    await credential.fill("discard-this-private-entry");
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await page.getByRole("button", { name: "Don't save" }).click();
    assert.equal(await credential.inputValue(), "");
}
