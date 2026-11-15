import { test } from "node:test";
import assert from "node:assert/strict";
import { pageFor, emit, calls, bounds } from "./browser.mjs";
import { states, frame } from "./fixtures.mjs";

const controls = [
    [states[0], "Start", { action: "tap", target: "primary" }],
    [states[8], "Allow", { action: "approve", id: "approval-1" }],
    [states[8], "Deny", { action: "deny", id: "approval-1" }],
    [states[10], "Relink", { action: "relink" }],
    [states[10], "Dismiss", { action: "dismiss" }],
    [states[11], "Dismiss", { action: "dismiss" }],
    [states[9], "Dismiss", { action: "dismiss" }],
];
test("F03 F04 F05 F18: exact POST at all inside corners/edges; outside edges emit none", async (t) => {
    const page = await pageFor(t);
    let seq = 0;
    for (const [wire, label, expected] of controls) {
        await emit(page, frame(++seq, wire));
        await bounds(page);
        const box = await page
            .getByRole("button", { name: label, exact: true })
            .boundingBox();
        const { x, y, width: w, height: h } = box;
        let before = (await calls(page)).length;
        for (const [dx, dy] of [
            [1, 1],
            [w - 1, 1],
            [1, h - 1],
            [w - 1, h - 1],
            [w / 2, 1],
            [w / 2, h - 1],
            [1, h / 2],
            [w - 1, h / 2],
        ]) {
            await page.mouse.click(x + dx, y + dy);
            const sent = await calls(page);
            assert.equal(sent.length, ++before);
            assert.deepEqual(sent.at(-1), {
                url: "http://127.0.0.1:4173/actions",
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify(expected),
            });
        }
        for (const [dx, dy] of [
            [-1, h / 2],
            [w + 1, h / 2],
            [w / 2, -1],
            [w / 2, h + 1],
        ])
            await page.mouse.click(x + dx, y + dy);
        assert.equal((await calls(page)).length, before);
        assert.equal(
            await page.locator("section").getAttribute("data-seq"),
            `${seq}`,
        );
    }
});
test("F10: old pointer-up cannot approve a replacement prompt", async (t) => {
    const page = await pageFor(t);
    await emit(page, frame(1, states[8]));
    const box = await page
        .getByRole("button", { name: "Allow", exact: true })
        .boundingBox();
    await page.mouse.move(box.x + 20, box.y + 20);
    await page.mouse.down();
    await emit(page, frame(2, states[8].replace("approval-1", "approval-2")));
    await page.mouse.up();
    assert.deepEqual(await calls(page), []);
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    assert.equal(
        (await calls(page))[0].body,
        '{"action":"approve","id":"approval-2"}',
    );
});
test("F09: pending, reject, timeout and authorization errors never retry or replace the prompt", async (t) => {
    const page = await pageFor(t);
    await emit(page, frame(1, states[8]));
    for (const status of ["reject", 401, 403, "pending"]) {
        await page.evaluate((value) => {
            window.postStatus = value;
        }, status);
        await page.getByRole("button", { name: "Allow", exact: true }).click();
        if (status === "pending") {
            assert.equal(
                await page
                    .getByRole("button", { name: "Allow", exact: true })
                    .isDisabled(),
                true,
            );
            await page.clock.runFor(10000);
        }
        assert.equal(
            await page.getByRole("status").textContent(),
            "Action could not be sent",
        );
        assert.equal(
            await page
                .getByRole("button", { name: "Allow", exact: true })
                .isEnabled(),
            true,
        );
        assert.equal(
            await page.locator("section p").textContent(),
            "Continue?",
        );
    }
    assert.equal((await calls(page)).length, 4);
    await page.evaluate(() => window.streams[0].onerror());
    assert.equal(
        await page.getByRole("status").textContent(),
        "Screen connection interrupted",
    );
    await emit(page, frame(2));
    assert.equal(await page.getByRole("status").textContent(), "");
    await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
    assert.equal(await page.evaluate(() => window.streams[0].closed), true);
});
test("F11: Tab, Shift-Tab, Enter and Space activate accessible native controls with visible focus", async (t) => {
    const page = await pageFor(t);
    let seq = 0;
    for (const [wire, label, expected] of controls) {
        await emit(page, frame(++seq, wire));
        if (JSON.parse(wire).relink === false)
            assert.equal(
                await page
                    .getByRole("button", { name: "Relink", exact: true })
                    .count(),
                0,
            );
        const target = page.getByRole("button", { name: label, exact: true });
        await target.focus();
        await page.keyboard.press("Tab");
        await page.keyboard.press("Shift+Tab");
        await target.focus();
        assert.equal(
            await target.evaluate(
                (node) => getComputedStyle(node).outlineWidth,
            ),
            "4px",
        );
        for (const key of ["Enter", "Space"]) {
            const before = (await calls(page)).length;
            await page.keyboard.press(key);
            assert.equal((await calls(page)).length, before + 1);
            assert.equal(
                (await calls(page)).at(-1).body,
                JSON.stringify(expected),
            );
        }
    }
});
