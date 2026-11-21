import assert from "node:assert/strict";
import { join } from "node:path";
import { chromium } from "playwright";

const [snapshotUrl, phase] = process.argv.slice(2);
assert.ok(["legacy", "registered"].includes(phase));
const legacy = phase === "legacy";
const { origin, search } = new URL(snapshotUrl);
assert.equal(new URL(origin).hostname, "127.0.0.1");
const pair = new URLSearchParams({ events: "/events" + search, actions: "/actions" + search });
const browser = await chromium.launch({ headless: true, args: ["--disable-background-networking", "--no-proxy-server"] });
try {
    const context = await browser.newContext({ viewport: { width: 960, height: 600 }, serviceWorkers: "block" });
    await context.route("**/*", (route) =>
        new URL(route.request().url()).origin === origin ? route.continue() : route.abort()
    );
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    const assets = page.waitForResponse((response) => new URL(response.url()).pathname === "/screen.js");
    await page.goto(origin + "/?" + pair);
    assert.equal((await assets).status(), 200);
    const read = page.waitForResponse((response) => new URL(response.url()).pathname === "/settings/v2");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const response = await read;
    assert.equal(response.status(), 200);
    const body = await response.json();
    assert.equal(body.portos.relink_advised, legacy ? true : undefined);
    assert.equal(JSON.stringify(body).includes("client_id"), false);
    const account = page.getByRole("region", { name: "Port OS", exact: true });
    const unlink = account.getByRole("button", { name: "Remove Port OS link" });
    await unlink.waitFor();
    const note = account.getByText("Relink to use this Echo's own Port OS client", { exact: true });
    assert.equal(await note.isVisible(), legacy);
    await unlink.focus();
    await page.keyboard.press("Tab");
    assert.equal(await page.getByRole("button", { name: "Load screen packs" }).evaluate((node) => node === document.activeElement), true);
    assert.equal(await account.evaluate((node) => node.scrollWidth <= node.clientWidth), true);
    await page.screenshot({ path: join(process.env.RUNNER_TEMP, "own-client-" + phase + ".png") });
    console.log("Paired Chromium", phase, "960x600 advice=" + (legacy ? "visible" : "hidden"), "client_id=absent next_focus=Load screen packs");
} finally {
    await browser.close();
}
