import { test } from "node:test";
import assert from "node:assert/strict";
import { pageFor, emit, calls, bounds } from "./browser.mjs";
import { frame, states } from "./fixtures.mjs";

test("F14 F15: exact long Unicode, quotes, markup and u64 expiry; scrolling leaves controls visible", async (t) => {
    const page = await pageFor(t);
    const long = 'é🌈"<script>bad()</script>\\'.repeat(100);
    for (const expiry of ["0", "18446744073709551615"]) {
        await emit(
            page,
            frame(
                expiry === "0" ? 1 : 2,
                `{"state":"link_code","user_code":${JSON.stringify(long)},"verification_uri":${JSON.stringify(long)},"expires_at_unix":${expiry}}`,
            ),
        );
        assert.deepEqual(await page.locator("section p").allTextContents(), [
            long,
            long,
            expiry,
        ]);
        await page.locator("section p").last().scrollIntoViewIfNeeded();
        assert.equal(await page.locator("section p").last().isVisible(), true);
    }
    const id = 'quoted"\\🌈\n<script>';
    const deny = "No 🌈".repeat(200);
    const approval = JSON.stringify({
        state: "approval",
        id,
        prompt: long,
        accept_label: long,
        deny_label: deny,
    });
    await emit(page, frame(3, approval));
    assert.equal(await page.locator("section p").textContent(), long);
    assert.deepEqual(await page.locator("button").allTextContents(), [
        long,
        deny,
    ]);
    await bounds(page, true);
    await page.keyboard.press("Tab");
    if (
        await page.evaluate(
            () => document.activeElement.className === "details",
        )
    )
        await page.keyboard.press("Tab");
    assert.equal(
        await page
            .locator("button")
            .first()
            .evaluate((node) => node === document.activeElement),
        true,
    );
    await page.keyboard.press("Enter");
    assert.equal(
        (await calls(page))[0].body,
        JSON.stringify({ action: "approve", id }),
    );
    await page.keyboard.press("Tab");
    await page.keyboard.press("Space");
    assert.equal(
        (await calls(page))[1].body,
        JSON.stringify({ action: "deny", id }),
    );
    assert.equal(
        await page.locator("section script,section img,section a").count(),
        0,
    );
    await emit(
        page,
        frame(
            4,
            JSON.stringify({
                state: "emotion",
                name: long,
                frames: ["https://example.invalid/image.png"],
                frame_ms: 80,
            }),
        ),
    );
    assert.equal(
        await page.locator('[aria-label="Animation frame"]').textContent(),
        "https://example.invalid/image.png",
    );
    assert.equal(await page.locator("section img,section iframe").count(), 0);
    await bounds(page, true);
    for (const [index, state] of [
        { state: "ready", message: long },
        { state: "error", message: long, relink: true },
    ].entries()) {
        await emit(page, frame(5 + index, JSON.stringify(state)));
        assert.deepEqual(
            await page.locator(".details p").allTextContents(),
            state.state === "ready" ? [] : [long],
        );
        await bounds(page, true);
    }
    console.log(
        "F14/F15: exact long strings/id/expiry; zero executable nodes or resource requests",
    );
});

test("F16: actual forward/reverse focus order, names, focus ring and keyboard actions for every control", async (t) => {
    const page = await pageFor(t);
    for (const [seq, wire] of states.entries()) {
        await emit(page, frame(seq + 1, wire));
        const targets = await page.getByRole("button").all();
        const before = (await calls(page)).length;
        for (const target of targets) {
            await page.keyboard.press("Tab");
            assert.equal(
                await target.evaluate(
                    (node) => node === document.activeElement,
                ),
                true,
            );
            assert.equal(
                await target.evaluate(
                    (node) => getComputedStyle(node).outlineWidth,
                ),
                "4px",
            );
            await page.keyboard.press("Enter");
        }
        assert.equal((await calls(page)).length, before + targets.length);
        if (targets.length > 1) {
            await page.keyboard.press("Shift+Tab");
            assert.equal(
                await targets[0].evaluate(
                    (node) => node === document.activeElement,
                ),
                true,
            );
            await page.keyboard.press("Space");
            assert.equal(
                (await calls(page)).length,
                before + targets.length + 1,
            );
        }
        await page.keyboard.press("Tab");
        if (targets.length === 0)
            assert.equal(
                await page.evaluate(() => document.activeElement.tagName),
                "BODY",
            );
    }
});

test("F17: reconnect replay keeps order; replacement/unload abort pending action and clear every timer", async (t) => {
    const page = await pageFor(t);
    await emit(page, frame(10, states[9]));
    await page.evaluate(() => window.streams[0].onerror());
    await emit(page, frame(9));
    assert.equal(await page.locator("h1").textContent(), "emotion");
    assert.equal(
        await page.getByRole("status").textContent(),
        "Screen connection interrupted",
    );
    await emit(page, frame(11, states[8]));
    assert.equal(await page.getByRole("status").textContent(), "");
    assert.equal(await page.evaluate(() => window.timers.size), 0);
    await page.evaluate(() => (window.postStatus = "pending"));
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    await emit(page, frame(12));
    assert.equal(await page.evaluate(() => window.aborted), 1);
    assert.equal(await page.evaluate(() => window.timers.size), 1);
    await page.getByRole("button", { name: "Begin session" }).click();
    await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
    assert.equal(await page.evaluate(() => window.aborted), 2);
    assert.equal(await page.evaluate(() => window.timers.size), 0);
    assert.equal(
        await page.evaluate(
            () => window.streams.filter((stream) => !stream.closed).length,
        ),
        0,
    );
    await page.evaluate(() => {
        window.resolvePost({ status: 204 });
        window.streams[0].onerror();
    });
    await page.clock.runFor(10000);
    assert.equal((await calls(page)).length, 2);
    assert.equal(
        await page.getByRole("status").textContent(),
        "Sending action…",
    );
});
