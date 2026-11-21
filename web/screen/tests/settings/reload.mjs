import { test } from "node:test";
import assert from "node:assert/strict";
import { pageFor, emit } from "../browser.mjs";
import { frame } from "../fixtures.mjs";
import { response, snapshot } from "./page.mjs";

const query =
    "?events=%2Fevents%3Ftoken%3Dsynthetic-screen-pair&actions=%2Factions%3Ftoken%3Dsynthetic-screen-pair";
const LOADED = "a".repeat(16);
const NEXT = "b".repeat(16);
const document = (build) =>
    `<!doctype html><html><head><meta name="echo-build" content="${build}" /></head></html>`;

// The harness page is not served, so it cannot navigate. Its session storage
// records the build a reload was asked for and then refuses it, which is the
// page's own rule for a storage that cannot remember: no reload.
async function configure(page) {
    await page.evaluate((loaded) => {
        const meta = window.document.createElement("meta");
        meta.name = "echo-build";
        meta.content = loaded;
        window.document.head.append(meta);
        const Stream = window.EventSource;
        window.EventSource = class extends Stream {
            addEventListener(name, listener) {
                (this.listeners ??= {})[name] = listener;
            }
            removeEventListener(name) {
                delete this.listeners[name];
            }
        };
        window.reloads = [];
        window.checks = [];
        let stored = null;
        Object.defineProperty(window, "sessionStorage", {
            value: {
                getItem: () => stored,
                setItem(key, value) {
                    stored = value;
                    window.reloads.push({ key, value });
                    throw new Error("the harness page stays");
                },
            },
        });
        const fetch = window.fetch;
        window.fetch = (url, options) => {
            if (new URL(url).pathname !== "/") return fetch(url, options);
            window.checks.push({ url, cache: options.cache });
            return window.served === "reject"
                ? Promise.reject(new Error("controlled"))
                : Promise.resolve({
                      status: window.served.status,
                      text: async () => window.served.body,
                  });
        };
    }, LOADED);
}
// The events stream opens (again) against a server whose document is `body`.
async function reconnect(page, served) {
    await page.evaluate(async (served) => {
        window.served = served;
        window.streams[0].listeners.open();
        // The check is one fetch and one body read.
        for (let turn = 0; turn < 8; turn++) await Promise.resolve();
    }, served);
}
const reloads = (page) => page.evaluate(() => window.reloads);

test("page build: only a different build named by the bridge asks for a reload, once", async (t) => {
    const page = await pageFor(t, query, false, configure);
    await emit(page, frame(1));
    for (const served of [
        { status: 200, body: document(LOADED) },
        // An older bridge names no build.
        { status: 200, body: "<!doctype html><html><head></head></html>" },
        { status: 200, body: document("") },
        { status: 503, body: document(NEXT) },
        "reject",
    ])
        await reconnect(page, served);
    assert.deepEqual(await reloads(page), []);
    assert.deepEqual(await page.evaluate(() => window.checks), [
        ...Array(5).fill({ url: "http://127.0.0.1:4173/", cache: "no-store" }),
    ]);
    await reconnect(page, { status: 200, body: document(NEXT) });
    assert.deepEqual(await reloads(page), [
        { key: "echo-screen-reload", value: NEXT },
    ]);
    // The build it already reloaded for is never reloaded for again.
    await reconnect(page, { status: 200, body: document(NEXT) });
    assert.deepEqual(await reloads(page), [
        { key: "echo-screen-reload", value: NEXT },
    ]);
    assert.equal(
        await page
            .locator('[aria-label="Screen presentation"]')
            .getAttribute("data-seq"),
        "1",
    );
});

test("page build: a page that names no build never asks the bridge", async (t) => {
    const page = await pageFor(t, query);
    assert.equal(
        await page.evaluate(() => "listeners" in window.streams[0]),
        false,
    );
});

test("page build: unsaved Settings edits postpone the reload until they are gone", async (t) => {
    const page = await pageFor(t, query, true, configure);
    await emit(page, frame(1));
    await response(page, snapshot);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.locator('.settings-fields[aria-busy="false"]').waitFor();
    const voice = page.getByLabel("voice.md", { exact: true });
    await voice.fill("Changed");
    await reconnect(page, { status: 200, body: document(NEXT) });
    await page.clock.runFor(5000);
    assert.deepEqual(await reloads(page), []);
    assert.equal(await voice.inputValue(), "Changed");
    // Putting the value back is no edit; the next look reloads.
    await voice.fill(snapshot.prompts.voice);
    assert.deepEqual(await reloads(page), []);
    await page.clock.runFor(1000);
    assert.deepEqual(await reloads(page), [
        { key: "echo-screen-reload", value: NEXT },
    ]);
    // Leaving with "Don't save" also frees a postponed reload.
    await page.evaluate(() => {
        window.reloads = [];
    });
    const other = "c".repeat(16);
    await voice.fill("Changed again");
    await reconnect(page, { status: 200, body: document(other) });
    await page.clock.runFor(2000);
    assert.deepEqual(await reloads(page), []);
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await page.getByRole("button", { name: "Don't save", exact: true }).click();
    await page.clock.runFor(1000);
    assert.deepEqual(await reloads(page), [
        { key: "echo-screen-reload", value: other },
    ]);
});
