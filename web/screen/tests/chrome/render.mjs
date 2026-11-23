import { test } from "node:test";
import assert from "node:assert/strict";
import { pageFor, emit } from "../browser.mjs";
import { frame } from "../fixtures.mjs";
import { snapshot as screenshot } from "../support/snapshot.mjs";
import { response, snapshot } from "../settings/page.mjs";

for (const [width, height, gripHeight, bandHeight] of [
    [1280, 800, 16, 10],
    [960, 600, 12, 7.5],
]) {
    test(`B06/B10: A2 chrome preserves reduced-motion, focus and edge geometry at ${width}×${height}`, async (t) => {
        const page = await pageFor(t, undefined, true, undefined, {
            viewport: { width, height },
            reducedMotion: "reduce",
        });
        await page.locator("html").evaluate((node) => {
            node.dataset.echoScreenChrome = "page";
        });
        const waiting = page
            .getByRole("status")
            .filter({ hasText: "Waiting for bridge" });
        await waiting.waitFor();
        if (width === 960) await screenshot(page, "a2-chrome-waiting-960x600");
        const speaking = frame(12, '{"state":"speaking"}');
        await emit(page, speaking);
        const band = page.locator(".screen-band");
        const grip = page.locator(".screen-grip");
        assert.deepEqual(await band.boundingBox(), {
            x: 0,
            y: height - bandHeight,
            width,
            height: bandHeight,
        });
        assert.deepEqual(await grip.boundingBox(), {
            x: 0,
            y: 0,
            width,
            height: gripHeight,
        });
        for (const strip of [band, grip]) {
            assert.equal(await strip.getAttribute("aria-hidden"), "true");
            assert.equal(
                await strip.evaluate(
                    (node) => getComputedStyle(node).pointerEvents,
                ),
                "none",
            );
        }
        assert.equal(
            await band.evaluate(
                (node) => getComputedStyle(node).backgroundColor,
            ),
            "rgb(0, 255, 255)",
        );
        assert.equal(
            await band.evaluate((node) => node.getAnimations().length),
            0,
        );
        assert.equal(
            await page.getByRole("heading", { name: "speaking" }).count(),
            1,
        );
        if (width === 1280)
            await screenshot(page, "a2-chrome-speaking-1280x800");

        const gear = page.getByRole("button", {
            name: "Settings",
            exact: true,
        });
        assert.equal((await gear.boundingBox()).y, gripHeight);
        await response(page, snapshot);
        await page.keyboard.press("Tab");
        assert.equal(
            await gear.evaluate((node) => node === document.activeElement),
            true,
        );
        await page.keyboard.press("Enter");
        const back = page.getByRole("button", { name: "Back", exact: true });
        assert.equal(
            await back.evaluate((node) => node === document.activeElement),
            true,
        );
        await page.keyboard.press("Enter");
        assert.equal(
            await gear.evaluate((node) => node === document.activeElement),
            true,
        );

        await page.evaluate(() => window.streams[0].onerror());
        assert.equal(await waiting.textContent(), "Waiting for bridge");
        assert.equal(
            await gear.evaluate((node) => node === document.activeElement),
            true,
        );
        assert.equal(await band.getAttribute("class"), "screen-band");
        await page.evaluate(() => window.streams[0].onopen());
        assert.equal(
            await waiting.count(),
            1,
            "an empty reopened stream still waits",
        );
        await emit(page, speaking);
        assert.equal(
            await waiting.count(),
            0,
            "a replay of the current frame restores chrome",
        );
        assert.equal(
            await band.getAttribute("class"),
            "screen-band band-speaking",
        );
        await page.evaluate(() => window.streams[0].onopen());
        await emit(page, frame(1));
        assert.equal(await band.getAttribute("class"), "screen-band");
        console.log(
            `A2 ${width}×${height}: grip ${gripHeight}px, band ${bandHeight}px, reduced motion, keyboard Settings/Back, waiting preserves focus, replay and reset recover`,
        );
    });
}
