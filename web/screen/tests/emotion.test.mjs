import { test } from "node:test";
import assert from "node:assert/strict";
import { animate } from "../dist/lib/emotion.js";
import { pageFor, emit, bounds, calls } from "./browser.mjs";
import { snapshot } from "./support/snapshot.mjs";
import { states, frame } from "./fixtures.mjs";

test("F12 F18: Emotion a,a,b,a at 0/79/80/160ms; Dismiss awaits authoritative replacement", async (t) => {
    const page = await pageFor(t);
    assert.equal(await page.evaluate(() => Date.now()), 0);
    await emit(page, frame(1, states[9]));
    const shown = () =>
        page.locator('[aria-label="Animation frame"]').textContent();
    assert.equal(await shown(), "frame-a");
    const first = await snapshot(page, "emotion");
    await bounds(page);
    assert.equal(await page.evaluate(() => Date.now()), 0);
    await page.clock.runFor(79);
    assert.equal(await page.evaluate(() => Date.now()), 79);
    assert.equal(await shown(), "frame-a");
    await page.clock.runFor(1);
    assert.equal(await page.evaluate(() => Date.now()), 80);
    assert.equal(await shown(), "frame-b");
    assert.notEqual(await snapshot(page, "emotion-b"), first);
    await page.getByRole("button", { name: "Dismiss" }).click();
    assert.equal((await calls(page))[0].body, '{"action":"dismiss"}');
    assert.equal(await shown(), "frame-b");
    await page.clock.runFor(80);
    assert.equal(await page.evaluate(() => Date.now()), 160);
    assert.equal(await shown(), "frame-a");
    await emit(page, frame(2));
    assert.equal(await page.evaluate(() => window.timers.size), 1);
    await page.clock.runFor(160);
    assert.equal(await page.locator("[data-ui-state=ready]").count(), 1);
    assert.equal(await page.getByLabel("Animation frame").count(), 0);
});

test("F13: empty/single/zero/u64-max animations stay bounded and teardown stops callbacks", async (t) => {
    const page = await pageFor(t);
    let seq = 0;
    for (const [frames, duration, expected, timers] of [
        [[], "80", "No animation frames", 0],
        [["only"], "80", "only", 0],
        [["first", "second"], "0", "first", 0],
        [["first", "second"], "18446744073709551615", "first", 1],
    ]) {
        await emit(
            page,
            frame(
                ++seq,
                `{"state":"emotion","name":"edge","frames":${JSON.stringify(frames)},"frame_ms":${duration}}`,
            ),
        );
        await page.clock.runFor(160);
        assert.equal(
            await page.locator('[aria-label="Animation frame"]').textContent(),
            expected,
        );
        assert.equal(await page.evaluate(() => window.timers.size), timers);
    }
    await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
    assert.equal(await page.evaluate(() => window.timers.size), 0);
    await emit(page, frame(++seq));
    assert.equal(await page.locator("h1").textContent(), "emotion");
});

test("large durations count exact bounded chunks; cancellation guards already queued callbacks", () => {
    const jobs = new Map();
    let id = 0;
    const clock = {
        setTimeout: (callback, ms) => {
            jobs.set(++id, { callback, ms });
            return id;
        },
        clearTimeout: (timer) => jobs.delete(timer),
    };
    const values = [];
    const stop = animate(
        ["a", "b"],
        4294967295n,
        (value) => values.push(value),
        clock,
    );
    function tick(expected) {
        const [key, job] = jobs.entries().next().value;
        assert.equal(job.ms, expected);
        jobs.delete(key);
        job.callback();
    }
    tick(2147483647);
    tick(2147483647);
    assert.deepEqual(values, ["a"]);
    tick(1);
    assert.deepEqual(values, ["a", "b"]);
    const queued = jobs.values().next().value.callback;
    stop();
    queued();
    assert.equal(jobs.size, 0);
    assert.deepEqual(values, ["a", "b"]);
});
