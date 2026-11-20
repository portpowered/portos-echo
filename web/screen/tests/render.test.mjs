import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { snapshot as pixelSnapshot } from "./support/snapshot.mjs";
import { pageFor, emit, calls, bounds } from "./browser.mjs";
import { states, frame } from "./fixtures.mjs";
import "./settings/usability.mjs";
import "./settings/page.mjs";
import "./settings/page-v2.mjs";
import "./settings/accounts.mjs";
import "./settings/packs.mjs";
import "./packs/player.mjs";
import "./packs/page.mjs";

test("F01 F02 F11: exact state, link and error values, semantic snapshots and reference bounds", async (t) => {
    const page = await pageFor(t);
    const golden = new URL("goldens/states.json", import.meta.url);
    const snapshots = JSON.parse(await readFile(golden, "utf8"));
    for (const [index, wire] of states.entries()) {
        const state = JSON.parse(wire);
        if (state.state === "link_code")
            await page.clock.setSystemTime(
                state.expires_at_unix * 1000 - 540000,
            );
        await emit(page, frame(index + 1, wire));
        const presentation = page.getByRole("region", {
            name: "Screen presentation",
        });
        assert.equal(
            await presentation.getAttribute("data-ui-state"),
            state.state,
        );
        const heading = presentation.getByRole("heading");
        if (["ready", "connected"].includes(state.state))
            assert.equal(await heading.count(), 0);
        else
            assert.equal(
                await heading.textContent(),
                state.state.replaceAll("_", " "),
            );
        assert.equal(
            await page.locator("section").getAttribute("data-seq"),
            `${index + 1}`,
        );
        if (state.state === "link_code")
            assert.deepEqual(
                await page.locator(".details p").allTextContents(),
                [state.user_code, state.verification_uri, "Expires in 9 min"],
            );
        for (const key of ["message", "prompt", "name"])
            if (key in state && state.state !== "ready")
                assert.equal(
                    await page
                        .locator(".details p")
                        .filter({ hasText: `${state[key]}` })
                        .count(),
                    1,
                );
        await bounds(page);
        const snapshot = await page.locator("main").ariaSnapshot();
        const name = `${state.state}${state.state === "error" ? `-${state.relink}` : ""}`;
        if (process.env.UPDATE_SCREEN_SNAPSHOTS === "1") {
            snapshots[name] = snapshot + "\n";
            await writeFile(golden, JSON.stringify(snapshots, null, 2) + "\n");
        }
        assert.equal(snapshot + "\n", snapshots[name]);
        await pixelSnapshot(page, name);
    }
    assert.equal(await page.locator("a").count(), 0);
    assert.equal((await calls(page)).length, 0);
});
test("home at 960x600: bottom clock rolls over, prompts have space, and Settings stays visible", async (t) => {
    const page = await pageFor(t, undefined, true, undefined, {
        viewport: { width: 960, height: 600 },
    });
    await page.clock.setSystemTime(new Date("2026-11-21T23:58:59Z"));
    await emit(page, frame(1));
    const clock = page.locator(".home-clock");
    const readings = () => clock.locator("p").allTextContents();
    assert.deepEqual(await readings(), ["11:58 PM", "11/21/2026"]);
    const home = await clock.boundingBox();
    assert.equal(home.x + home.width / 2, 480);
    assert.ok(home.y >= 480 && home.y + home.height <= 600);
    assert.equal(
        await clock.evaluate((node) => getComputedStyle(node).textAlign),
        "center",
    );
    const gear = await page
        .getByRole("button", { name: "Settings", exact: true })
        .boundingBox();
    assert.ok(gear.x >= 840 && gear.y < 48);
    assert.equal(
        await page.getByText(/^(ready|echo live ready|start)$/i).count(),
        0,
    );
    await page.screenshot({
        path: "test-results/screenshots/home-960x600.png",
    });
    await page.clock.runFor(1000);
    assert.deepEqual(await readings(), ["11:59 PM", "11/21/2026"]);
    await page.clock.runFor(60000);
    assert.deepEqual(await readings(), ["12:00 AM", "11/22/2026"]);

    await emit(page, frame(2, states[2]));
    assert.equal(await page.locator("[data-ui-state=connected]").count(), 1);
    assert.equal(await page.getByText(/^connected$/i).count(), 0);
    assert.deepEqual(await readings(), ["12:00 AM", "11/22/2026"]);

    for (const [seq, wire] of [
        [3, states[7]],
        [4, states[8]],
    ]) {
        const state = JSON.parse(wire);
        await emit(page, frame(seq, wire));
        assert.equal(await clock.count(), 0);
        const card = page.locator(".details");
        const box = await card.boundingBox();
        assert.ok(box.y >= 0 && box.y + box.height <= home.y);
        assert.deepEqual(
            await card.locator("p").allTextContents(),
            state.state === "link_code"
                ? [state.user_code, state.verification_uri, "Expired"]
                : [state.prompt],
        );
        if (state.state === "approval") {
            for (const name of [state.accept_label, state.deny_label]) {
                const target = await page
                    .getByRole("button", { name, exact: true })
                    .boundingBox();
                assert.ok(
                    target.y >= box.y + box.height &&
                        target.y + target.height <= home.y,
                );
            }
        }
        await page.screenshot({
            path: `test-results/screenshots/${state.state}-960x600.png`,
        });
    }
    await emit(page, frame(5));
    assert.deepEqual(await clock.boundingBox(), home);
    console.log(
        "960x600 home: zero redundant labels, bottom clock/date, top-right Settings, minute and midnight rollover, prompts above the clock area",
    );
});
test("Link Code refreshes in place at minute boundaries and expires at the deadline", async (t) => {
    const page = await pageFor(t);
    const state = JSON.parse(states[7]);
    const deadline = state.expires_at_unix * 1000;
    await page.clock.setSystemTime(deadline - 540000);
    await emit(page, frame(1, states[7]));

    const details = page.locator(".details");
    const lines = details.locator("p");
    const expiry = await lines.last().elementHandle();
    assert.deepEqual(await lines.allTextContents(), [
        state.user_code,
        state.verification_uri,
        "Expires in 9 min",
    ]);
    assert.doesNotMatch(await details.innerText(), /\d{10}/);
    assert.equal(await details.locator("a").count(), 0);
    await page.clock.runFor(60000);
    assert.equal(await expiry.textContent(), "Expires in 8 min");
    assert.equal(await expiry.evaluate((node) => node.isConnected), true);
    await page.clock.runFor(480000 - 1);
    assert.equal(await expiry.textContent(), "Expires in 1 min");
    await page.clock.runFor(1);
    assert.equal(await expiry.textContent(), "Expired");
    assert.equal(await page.evaluate(() => window.timers.size), 0);

    await page.clock.runFor(1000);
    await emit(page, frame(2, states[7]));
    await page.clock.runFor(60000);
    assert.deepEqual(await lines.allTextContents(), [
        state.user_code,
        state.verification_uri,
        "Expired",
    ]);
    assert.equal(await page.evaluate(() => window.timers.size), 0);
    assert.deepEqual(await calls(page), []);
});

test("Link Code cancels its refresh on replacement and page disposal", async (t) => {
    const deadline = JSON.parse(states[7]).expires_at_unix * 1000;
    const page = await pageFor(t);
    await page.clock.setSystemTime(deadline - 540000);
    await emit(page, frame(1, states[7]));
    const expiry = await page.locator(".details p").last().elementHandle();
    assert.equal(await page.evaluate(() => window.timers.size), 1);
    await emit(page, frame(2));
    assert.equal(await page.evaluate(() => window.timers.size), 1);
    await page.clock.runFor(600000);
    assert.equal(await expiry.textContent(), "Expires in 9 min");
    assert.deepEqual(await page.locator(".home-clock p").allTextContents(), [
        "10:14 PM",
        "11/14/2023",
    ]);
    assert.deepEqual(await calls(page), []);

    const unloaded = await pageFor(t);
    await unloaded.clock.setSystemTime(deadline - 540000);
    await emit(unloaded, frame(1, states[7]));
    assert.equal(await unloaded.evaluate(() => window.timers.size), 1);
    await unloaded.evaluate(() => window.dispatchEvent(new Event("pagehide")));
    assert.equal(await unloaded.evaluate(() => window.timers.size), 0);
    await unloaded.clock.runFor(600000);
    assert.equal(
        await unloaded.locator(".details p").last().textContent(),
        "Expires in 9 min",
    );
    assert.deepEqual(await calls(unloaded), []);
});

test("F06 F07: lossless monotonic rendering, invalid update recovery and inert HTML", async (t) => {
    const page = await pageFor(t);
    await emit(page, frame(9007199254740993n));
    for (const n of [9007199254740992n, 9007199254740993n])
        await emit(page, frame(n, states[1]));
    assert.equal(await page.locator("[data-ui-state=ready]").count(), 1);
    for (const wire of ["bad", '{"seq":9,"state":{"state":"unknown"}}']) {
        await emit(page, wire);
        assert.equal(
            await page.getByRole("status").textContent(),
            "Screen update could not be read",
        );
        assert.equal(
            await page.locator("section").getAttribute("data-seq"),
            "9007199254740993",
        );
    }
    await emit(
        page,
        frame(
            18446744073709551615n,
            '{"state":"ready","message":"<img src=x onerror=alert(1)>"}',
        ),
    );
    assert.equal(
        await page.locator("section").getAttribute("data-seq"),
        "18446744073709551615",
    );
    assert.equal(await page.locator("img").count(), 0);
    assert.equal(await page.getByRole("status").textContent(), "");
});
test("F08: invalid bootstrap performs zero stream constructions and zero POSTs", async (t) => {
    for (const query of [
        "",
        "?events=/e",
        "?events=https://example.invalid&actions=/a",
        "?events=data:x&actions=/a",
    ]) {
        const page = await pageFor(t, query);
        assert.equal(
            await page.getByRole("status").textContent(),
            "Screen configuration is invalid",
        );
        assert.equal(await page.evaluate(() => window.streams.length), 0);
        assert.deepEqual(await calls(page), []);
    }
});
