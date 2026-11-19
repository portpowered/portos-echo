import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { snapshot as pixelSnapshot } from "./support/snapshot.mjs";
import { pageFor, emit, calls, bounds } from "./browser.mjs";
import { states, frame } from "./fixtures.mjs";
import "./settings/page.mjs";

test("F01 F02 F11: exact supplied status/link/error values, semantic snapshots and reference bounds", async (t) => {
    const page = await pageFor(t);
    const golden = new URL("goldens/states.json", import.meta.url);
    const snapshots = JSON.parse(await readFile(golden, "utf8"));
    for (const [index, wire] of states.entries()) {
        await emit(page, frame(index + 1, wire));
        const state = JSON.parse(wire);
        assert.equal(
            await page.locator("h1").textContent(),
            state.state.replaceAll("_", " "),
        );
        assert.equal(
            await page.locator("section").getAttribute("data-seq"),
            `${index + 1}`,
        );
        for (const key of [
            "message",
            "user_code",
            "verification_uri",
            "expires_at_unix",
            "prompt",
            "name",
        ])
            if (key in state)
                assert.equal(
                    await page
                        .locator("section p")
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
test("F06 F07: lossless monotonic rendering, invalid update recovery and inert HTML", async (t) => {
    const page = await pageFor(t);
    await emit(page, frame(9007199254740993n));
    for (const n of [9007199254740992n, 9007199254740993n])
        await emit(page, frame(n, states[1]));
    assert.equal(await page.locator("h1").textContent(), "ready");
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
