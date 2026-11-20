import { test } from "node:test";
import assert from "node:assert/strict";
import { settingsPage, response } from "./page.mjs";
import { calls } from "../browser.mjs";

const empty = {
    version: 1,
    revision: 0,
    active: null,
    warning: null,
    installed: [],
    jobs: [],
};
const base = "http://127.0.0.1:4173/screen-packs/v1";
const token = "?token=synthetic-screen-pair";
const status = (page) => page.locator(".settings-panel [role=status]");
async function load(page, data = empty) {
    await response(page, data);
    await page
        .getByRole("button", { name: /^(Load screen packs|Refresh packs)$/ })
        .click();
    assert.equal(await status(page).textContent(), "Screen packs loaded");
}

test("packs: explicit empty catalog, queued install, safe failure and pairing states", async (t) => {
    const page = await settingsPage(t);
    await load(page);
    assert.equal(
        await page
            .getByText("No screen packs installed", { exact: true })
            .count(),
        1,
    );
    assert.equal(
        await page.getByLabel("Screen pack", { exact: true }).inputValue(),
        "",
    );
    assert.equal((await calls(page)).at(-1).url, base + token);
    const url = page.getByLabel("Pack HTTPS URL", { exact: true });
    const sha = page.getByLabel("Pack SHA-256", { exact: true });
    await url.fill("https://127.0.0.1/template.screenpack.tar.gz");
    await sha.fill("a".repeat(63));
    await page
        .getByRole("button", { name: "Download pack", exact: true })
        .click();
    assert.equal((await calls(page)).length, 2, "invalid digest stays local");
    await sha.fill("A".repeat(64));
    await response(
        page,
        { version: 1, message: "private URL must not appear" },
        422,
    );
    await page
        .getByRole("button", { name: "Download pack", exact: true })
        .click();
    assert.match(
        await status(page).textContent(),
        /^Screen pack request was refused/,
    );
    assert.equal(await sha.inputValue(), "A".repeat(64));
    await response(page, { version: 1, job: "7" }, 202);
    await page
        .getByRole("button", { name: "Download pack", exact: true })
        .click();
    assert.equal(
        await status(page).textContent(),
        "Download 7 queued. Refresh packs to check progress.",
    );
    assert.deepEqual((await calls(page)).at(-1), {
        url: base + "/install" + token,
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
            version: 1,
            url: "https://127.0.0.1/template.screenpack.tar.gz",
            sha256: "A".repeat(64),
        }),
    });
    assert.equal(await url.inputValue(), "");
    assert.equal(await sha.inputValue(), "");
    await load(page, {
        ...empty,
        jobs: [{ id: "7", status: "failed", reason: "checksum_mismatch" }],
    });
    assert.equal(
        await page
            .getByText("Download 7: failed (checksum mismatch)", {
                exact: true,
            })
            .count(),
        1,
    );
    for (const code of [401, 403, 503]) {
        await response(page, null, code);
        await page
            .getByRole("button", { name: "Refresh packs", exact: true })
            .click();
        assert.equal(
            await status(page).textContent(),
            code === 503
                ? "Screen packs are unavailable or busy. Refresh packs to try again."
                : "Pairing is unavailable. Reopen the paired screen.",
        );
    }
    const count = (await calls(page)).length;
    await page.clock.runFor(10000);
    assert.equal((await calls(page)).length, count, "no polling/retry");
});

test("packs: authenticated preview, layout-only, selection and focus preserve voice edits", async (t) => {
    const page = await settingsPage(t);
    // Record the img boundary without requesting media; decoding is separate proof.
    await page.evaluate(() => {
        window.previews = [];
        Object.defineProperty(HTMLImageElement.prototype, "src", {
            set(value) {
                window.previews.push(value);
            },
        });
    });
    const installed = [
        {
            id: "template@1.0.0",
            name: "template",
            version: "1.0.0",
            rest_url: "/screen-packs/v1/template@1.0.0/rest.png",
        },
        { id: "layout", name: "layout", version: "1.0.0", rest_url: null },
    ];
    await page
        .getByLabel("voice.md", { exact: true })
        .fill("Keep this unsaved edit");
    await load(page, { ...empty, active: installed[0].id, installed });
    assert.deepEqual(await page.evaluate(() => window.previews), [
        base + "/template@1.0.0/rest.png" + token,
    ]);
    assert.equal(
        await page.getByRole("img", { name: "template rest pose" }).count(),
        1,
    );
    assert.equal(
        await page
            .getByText("Layout only — no character preview", { exact: true })
            .count(),
        1,
    );
    await page
        .getByLabel("Screen pack", { exact: true })
        .selectOption("layout");
    // Route two sequential replies through the controlled fetch boundary.
    await page.evaluate(
        (snapshot) => {
            const fetch = window.fetch;
            window.fetch = (url, options) => {
                window.postStatus = 200;
                window.responseBody =
                    options.method === "POST"
                        ? { version: 1, restart_pending: true }
                        : snapshot;
                return fetch(url, options);
            };
        },
        { ...empty, revision: 1, active: "layout", installed },
    );
    await page
        .getByRole("button", { name: "Use screen pack", exact: true })
        .click();
    assert.equal(
        await status(page).textContent(),
        "Screen pack saved. Appearance updates without restarting the bridge.",
    );
    const selection = (await calls(page)).at(-2);
    assert.equal(selection.url, base + "/selection" + token);
    assert.deepEqual(JSON.parse(selection.body), {
        version: 1,
        screen_pack: "layout",
    });
    assert.equal(
        await page.getByLabel("Screen pack", { exact: true }).inputValue(),
        "layout",
    );
    assert.equal(
        await page.getByLabel("voice.md", { exact: true }).inputValue(),
        "Keep this unsaved edit",
    );
    assert.equal(
        await page
            .getByRole("button", { name: "Refresh packs", exact: true })
            .evaluate((node) => node === document.activeElement),
        true,
    );
    await page.getByLabel("Screen pack", { exact: true }).selectOption("");
    await page
        .getByRole("button", { name: "Use screen pack", exact: true })
        .click();
    assert.deepEqual(JSON.parse((await calls(page)).at(-2).body), {
        version: 1,
        screen_pack: "",
    });
});

test("packs: malformed catalog and escaping preview are inert, cancelled load cannot render", async (t) => {
    const page = await settingsPage(t);
    for (const value of [
        null,
        { ...empty, revision: -1 },
        { ...empty, active: "missing" },
        {
            ...empty,
            installed: [
                {
                    id: "template",
                    name: "template",
                    version: "1.0.0",
                    rest_url: "https://127.0.0.1/rest.png",
                },
            ],
        },
        {
            ...empty,
            jobs: [
                {
                    id: "1",
                    status: "failed",
                    reason: "https://127.0.0.1/private",
                },
            ],
        },
    ]) {
        await response(page, value);
        await page
            .getByRole("button", { name: "Load screen packs", exact: true })
            .click();
        assert.equal(
            await status(page).textContent(),
            "Pack response could not be read",
        );
        assert.equal(await page.locator(".settings-packs img").count(), 0);
    }
    await response(page, null, "pending");
    await page
        .getByRole("button", { name: "Load screen packs", exact: true })
        .click();
    assert.equal(await status(page).textContent(), "Loading screen packs…");
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await page.evaluate(
        (data) => window.resolvePost({ status: 200, json: async () => data }),
        empty,
    );
    assert.equal(await page.locator(".settings-panel").isVisible(), false);
    assert.equal(
        await page
            .getByText("No screen packs installed", { exact: true })
            .count(),
        0,
    );
    assert.equal(await page.evaluate(() => window.timers.size), 0);
});
