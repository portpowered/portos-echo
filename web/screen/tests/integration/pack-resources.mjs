import assert from "node:assert/strict";
import { cp, mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

// T3 native Chromium consumes suite-owned generated media and the prebuilt
// SettingsService driver. Only scheduling changes: every response is delivered
// by production HTTP, including stylesheet URLs, video ranges and previews.
export async function concurrentResources(t, open, playing, refresh) {
    const id = "template@0.1.0";
    const responses = [];
    const failures = [];
    const paired = new Set();
    let background;
    const f = await open(t, {
        initialState: { state: "ready", message: "Ready" },
        async prepare(home, generated) {
            await mkdir(join(home, "screens"));
            for (const name of ["template", "sample"]) {
                const pack = join(home, "screens", `${name}@0.1.0`);
                await cp(join(generated, id), pack, { recursive: true });
                const manifest = JSON.parse(
                    await readFile(join(pack, "pack.json"), "utf8"),
                );
                manifest.name = name;
                manifest.layout.css = "look.css";
                await writeFile(
                    join(pack, "pack.json"),
                    JSON.stringify(manifest),
                );
                await cp(join(pack, "rest.png"), join(pack, "background.png"));
                await writeFile(
                    join(pack, "look.css"),
                    ".pack-clock {color:#abcdef;background-image:url(background.png)}",
                );
            }
        },
        async beforeNavigate(page) {
            background = page.waitForResponse((response) =>
                new URL(response.url()).pathname.endsWith("/background.png"),
            );
            const held = [];
            // Release CSS and initial video together, without replacing their
            // resource boundary or depending on a lucky browser fetch order.
            await page.route("**/screen-packs/v1/**", async (route) => {
                const file = new URL(route.request().url()).pathname
                    .split("/")
                    .at(-1);
                if (
                    ["look.css", "idle.webm"].includes(file) &&
                    paired.size < 2
                ) {
                    paired.add(file);
                    held.push(route);
                    if (paired.size === 2)
                        await Promise.all(
                            held.map((pending) => pending.continue()),
                        );
                    return;
                }
                await route.continue();
            });
            page.on("response", (response) => {
                const url = new URL(response.url());
                if (url.pathname.startsWith("/screen-packs/v1/")) {
                    responses.push(url.pathname);
                    if (![200, 206].includes(response.status()))
                        failures.push([url.pathname, response.status()]);
                }
            });
        },
    });
    const { page } = f;
    await f.update({ state: "ready", message: "Ready" });
    await playing(page, "idle.webm");
    await page.waitForFunction(
        () =>
            getComputedStyle(document.querySelector(".pack-clock")).color ===
            "rgb(171, 205, 239)",
    );
    assert.equal((await background).status(), 200);
    assert.deepEqual([...paired].sort(), ["idle.webm", "look.css"]);
    assert.equal((await f.api("/screen-packs/v1")).active, id);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await refresh(page);
    const previews = page.locator(".settings-packs img");
    assert.equal(await previews.count(), 2);
    // Load even offscreen lazy previews together; no src/decoder/network mock.
    assert.deepEqual(
        await previews.evaluateAll(async (images) => {
            for (const image of images) image.loading = "eager";
            await Promise.all(images.map((image) => image.decode()));
            return images.map((image) => image.naturalWidth);
        }),
        [640, 640],
    );
    assert.equal(
        await page.getByText("Preview unavailable", { exact: true }).count(),
        0,
    );
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await playing(page, "idle.webm");
    assert.equal(
        await page.locator(".screen-pack .pack-stage").isVisible(),
        true,
    );
    assert.equal((await f.api("/screen-packs/v1")).active, id);
    assert.equal((await f.api("/settings/v1")).restart_pending, false);
    assert.deepEqual(failures, []);
    for (const path of [
        `/${id}/look.css`,
        `/${id}/idle.webm`,
        `/${id}/background.png`,
        `/${id}/rest.png`,
        "/sample@0.1.0/rest.png",
    ])
        assert.equal(
            responses.some((response) => response.endsWith(path)),
            true,
            path,
        );
    assert.deepEqual(f.outside, []);
    await mkdir("test-results/screenshots", { recursive: true });
    await page.screenshot({
        path: "test-results/screenshots/pack-concurrent-resources.png",
    });
    console.log(
        "R1 native: CSS + generated WebM + CSS background + 2 decoded previews; responses=200/206; selected template retained; restart_requests=0",
    );
}
