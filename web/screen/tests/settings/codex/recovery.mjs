import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { pageFor, emit, calls, bounds } from "../../browser.mjs";
import { frame, recoveryActions, recoveryStates } from "../../fixtures.mjs";

const challenge = JSON.parse(recoveryStates[1]);

async function expectChallenge(page, countdown) {
    assert.deepEqual(await page.locator(".details p").allTextContents(), [
        "Microphone muted",
        challenge.user_code,
        challenge.verification_uri,
        countdown,
    ]);
}

function transition(page, edge) {
    return page.evaluate((name) => window.streams[0][name](), edge);
}

test("B13: disconnect hides the code until an authoritative snapshot is replayed", async (t) => {
    const page = await pageFor(t);
    await page.clock.setSystemTime(1800000000000);
    const wire = frame(2, recoveryStates[1]);
    await emit(page, wire);
    await expectChallenge(page, "Expires in 5 min");
    await transition(page, "onerror");
    assert.deepEqual(await page.locator(".details p").allTextContents(), []);
    assert.equal(await page.getByRole("button", { name: "Cancel" }).count(), 0);
    await transition(page, "onopen");
    await emit(page, wire);
    await expectChallenge(page, "Expires in 5 min");
    await transition(page, "onerror");
    await transition(page, "onopen");
    await emit(page, frame(1, recoveryStates[0]));
    assert.deepEqual(await page.locator(".details p").allTextContents(), [
        "Microphone muted",
        "Relink to sign in to ChatGPT.",
    ]);
    assert.equal(await page.getByRole("button", { name: "Relink" }).count(), 1);
});

test("B14: 960 × 600 recovery has visible keyboard controls, exact actions and an expiring challenge", async (t) => {
    const page = await pageFor(t, undefined, false, undefined, {
        viewport: { width: 960, height: 600 },
    });
    await page.clock.setSystemTime(1800000000000);
    await emit(page, frame(1, recoveryStates[1]));
    await expectChallenge(page, "Expires in 5 min");
    await bounds(page);
    for (const [name, key, wire] of [
        ["Cancel", "Enter", recoveryActions[0]],
        ["Get a new code", "Space", recoveryActions[1]],
    ]) {
        await page.keyboard.press("Tab");
        const control = page.getByRole("button", { name, exact: true });
        assert.equal(
            await control.evaluate((node) => {
                const style = getComputedStyle(node);
                return (
                    document.activeElement === node &&
                    node.matches(":focus-visible") &&
                    style.outlineStyle !== "none" &&
                    parseFloat(style.outlineWidth) > 0
                );
            }),
            true,
        );
        await page.keyboard.press(key);
        await control.evaluate((node) => {
            if (node.disabled) throw new Error("Action did not settle");
        });
        assert.equal((await calls(page)).at(-1).body, wire);
    }
    await mkdir("test-results/screenshots", { recursive: true });
    await page.screenshot({
        path: "test-results/screenshots/chatgpt-signin-960x600.png",
    });
    await page.clock.runFor(60000);
    await expectChallenge(page, "Expires in 4 min");
    await emit(
        page,
        frame(
            2,
            JSON.stringify({
                ...JSON.parse(recoveryStates[0]),
                starting: true,
                message: "Getting a ChatGPT sign-in code…",
            }),
        ),
    );
    assert.deepEqual(await page.locator(".details p").allTextContents(), [
        "Microphone muted",
        "Getting a ChatGPT sign-in code…",
    ]);
    await emit(page, frame(3, recoveryStates[1]));
    await page.clock.runFor(240000);
    assert.deepEqual(await page.locator(".details p").allTextContents(), [
        "Microphone muted",
        "Expired",
    ]);
    assert.equal(await page.getByRole("button", { name: "Cancel" }).count(), 0);
    assert.equal((await calls(page)).length, 2);
    await emit(page, frame(4, recoveryStates[0]));
    assert.equal(
        await page
            .getByRole("button", { name: "Relink" })
            .evaluate((node) => document.activeElement === node),
        true,
    );
    await emit(page, frame(5));
    assert.equal(
        await page
            .getByRole("button", { name: "Begin session" })
            .evaluate((node) => document.activeElement === node),
        true,
    );
});
