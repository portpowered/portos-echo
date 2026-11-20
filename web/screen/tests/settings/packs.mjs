import { test } from "node:test";
import assert from "node:assert/strict";
import { settingsPage as basePage, response } from "./page.mjs";
import { calls } from "../browser.mjs";

async function settingsPage(t) {
    let page;
    // Capture the rendered controls before the shared context cleanup, including
    // failures. A bounded action reports its exact locator before the test cap.
    t.after(async () => {
        if (page)
            await page.screenshot({
                path: `test-results/screenshots/${t.name.replace(/[^a-z0-9]/gi, "-")}.png`,
            });
    });
    page = await basePage(t);
    page.setDefaultTimeout(5000);
    return page;
}

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
    assert.equal((await calls(page)).length, 3, "invalid digest stays local");
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
    assert.equal(await page.evaluate(() => window.timers.size), 1);
});

test("packs: pending load disables writes, preserves keyboard focus and reports unavailable previews safely", async (t) => {
    const page = await settingsPage(t);
    const refresh = page.getByRole("button", {
        name: "Load screen packs",
        exact: true,
    });
    await refresh.focus();
    await response(page, null, "pending");
    await page.keyboard.press("Enter");
    assert.equal(await status(page).textContent(), "Loading screen packs…");
    assert.equal(await refresh.isDisabled(), true);
    assert.equal(
        await page
            .getByRole("button", { name: "Save settings", exact: true })
            .isDisabled(),
        true,
    );
    assert.equal(
        await page.locator(".settings-fields").getAttribute("aria-busy"),
        "true",
    );
    // Observe preview failure without making any network request. Actual media
    // decoding through the real server belongs to the generated-pack journey.
    await page.evaluate(() => {
        Object.defineProperty(HTMLImageElement.prototype, "src", {
            set() {},
        });
    });
    await page.evaluate(
        (body) => window.resolvePost({ status: 200, json: async () => body }),
        {
            ...empty,
            warning: "invalid_selection",
            installed: [
                {
                    id: "template@1.0.0",
                    name: "template",
                    version: "1.0.0",
                    rest_url: "/screen-packs/v1/template@1.0.0/rest.png",
                },
            ],
        },
    );
    await page
        .getByRole("button", { name: "Refresh packs", exact: true })
        .waitFor();
    assert.equal(await status(page).textContent(), "Screen packs loaded");
    assert.equal(await refresh.count(), 0);
    const updated = page.getByRole("button", {
        name: "Refresh packs",
        exact: true,
    });
    assert.equal(
        await updated.evaluate((node) => node === document.activeElement),
        true,
    );
    assert.equal(await updated.isEnabled(), true);
    assert.equal(
        await page
            .getByText(
                "The selected pack is unavailable. Using the built-in appearance.",
                { exact: true },
            )
            .count(),
        1,
    );
    await page
        .getByRole("img", { name: "template rest pose" })
        .dispatchEvent("error");
    assert.equal(
        await page.getByText("Preview unavailable", { exact: true }).count(),
        1,
    );
    assert.equal(await page.getByRole("img").count(), 0);
    await page.keyboard.press("Tab");
    assert.equal(
        await page
            .getByLabel("Screen pack", { exact: true })
            .evaluate((node) => node === document.activeElement),
        true,
    );
    await page.keyboard.press("Tab");
    assert.equal(
        await page
            .getByRole("button", { name: "Use screen pack", exact: true })
            .evaluate((node) => node === document.activeElement),
        true,
    );
    assert.equal(
        (await calls(page)).length,
        3,
        "navigation never submits a write",
    );
});

test("packs: narrow and reference viewports retain scrollable native controls, contrast and touch targets", async (t) => {
    const page = await settingsPage(t);
    await load(page);
    for (const viewport of [
        { width: 1280, height: 800 },
        { width: 640, height: 600 },
    ]) {
        await page.setViewportSize(viewport);
        for (const control of await page
            .locator(
                ".settings-packs button, .settings-packs input, .settings-packs select",
            )
            .all()) {
            await control.scrollIntoViewIfNeeded();
            await control.evaluate((node) =>
                node.scrollIntoView({ block: "center" }),
            );
            const box = await control.boundingBox();
            const area = await page.locator(".settings-fields").boundingBox();
            assert.ok(box.width >= 64 && box.height >= 64, JSON.stringify(box));
            assert.ok(
                box.x >= area.x && box.x + box.width <= area.x + area.width,
                JSON.stringify({ box, area }),
            );
            assert.ok(
                box.y >= area.y && box.y + box.height <= area.y + area.height,
                JSON.stringify(box),
            );
        }
        for (const control of [
            page.getByRole("button", { name: "Back", exact: true }),
            status(page),
        ]) {
            const box = await control.boundingBox();
            assert.ok(
                box.y >= 0 && box.y + box.height <= viewport.height,
                JSON.stringify(box),
            );
        }
        const contrasts = await page
            .locator(".settings-packs")
            .evaluate((root) => {
                const luminance = (color) => {
                    const [r, g, b] = color
                        .match(/\d+/g)
                        .slice(0, 3)
                        .map(Number)
                        .map((n) => n / 255)
                        .map((v) =>
                            v <= 0.04045
                                ? v / 12.92
                                : ((v + 0.055) / 1.055) ** 2.4,
                        );
                    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
                };
                return [
                    ...root.querySelectorAll(
                        "h2,h3,p,label,button,input,select",
                    ),
                ].map((node) => {
                    let background = node;
                    while (
                        getComputedStyle(background).backgroundColor ===
                            "rgba(0, 0, 0, 0)" &&
                        background.parentElement
                    )
                        background = background.parentElement;
                    const a = luminance(getComputedStyle(node).color);
                    const b = luminance(
                        getComputedStyle(background).backgroundColor,
                    );
                    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
                });
            });
        for (const ratio of contrasts)
            assert.ok(ratio >= 4.5, `pack text contrast ${ratio}`);
        assert.deepEqual(
            await page.evaluate(() => ({
                width: document.documentElement.scrollWidth,
                height: document.documentElement.scrollHeight,
            })),
            viewport,
        );
        await page.screenshot({
            path: `test-results/screenshots/packs-${viewport.width}x${viewport.height}.png`,
        });
        console.log(
            `packs controls: ${viewport.width}x${viewport.height}; targets >=64px; minimum contrast ${Math.min(...contrasts).toFixed(2)}:1`,
        );
    }
});
