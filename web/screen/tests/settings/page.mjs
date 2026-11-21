import { test } from "node:test";
import assert from "node:assert/strict";
import { pageFor, emit, calls } from "../browser.mjs";
import { frame, states } from "../fixtures.mjs";

const query =
    "?events=%2Fevents%3Ftoken%3Dsynthetic-screen-pair&actions=%2Factions%3Ftoken%3Dsynthetic-screen-pair";
export const snapshot = {
    version: 1,
    settings: {
        voice: { session_listen_enabled: true, session_listen_seconds: 15 },
        codex: { voice: "default" },
        agent: { daily_request_cap: "9223372036854775807", max_tool_calls: 8 },
    },
    allowed_voices: ["default", "marin", "cedar"],
    prompts: { voice: "Voice instructions", runner: "Runner instructions" },
    link_status: "linked",
    bridge_version: "0.1.0",
    restart_pending: false,
    warnings: ["Settings overlay was ignored"],
};
export async function response(page, body, status = 200) {
    await page.evaluate(
        ({ body, status }) => {
            window.responseBody = body;
            window.postStatus = status;
        },
        { body, status },
    );
}
export async function settingsPage(t) {
    const page = await pageFor(t, query, true);
    await emit(page, frame(1));
    await response(page, snapshot);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.locator('.settings-fields[aria-busy="false"]').waitFor();
    // A successful load needs no message: the status line stays empty.
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        "",
    );
    return page;
}
test("settings: gear/back preserves the exact authoritative frame and focus, with no action", async (t) => {
    const page = await settingsPage(t);
    assert.equal(
        await page
            .getByRole("button", { name: "Back", exact: true })
            .evaluate((node) => node === document.activeElement),
        true,
    );
    assert.equal(
        await page.getByLabel("Daily request cap").inputValue(),
        "9223372036854775807",
    );
    assert.deepEqual(
        await page.getByLabel("Live voice").locator("option").allTextContents(),
        snapshot.allowed_voices,
    );
    assert.equal(
        await page.getByText("Port OS: linked", { exact: true }).count(),
        1,
    );
    assert.equal(
        await page.getByText("Version: 0.1.0", { exact: true }).count(),
        1,
    );
    const before = await page
        .locator('[aria-label="Screen presentation"]')
        .innerHTML();
    await page.getByRole("button", { name: "Back", exact: true }).click();
    assert.equal(
        await page.locator('[aria-label="Screen presentation"]').innerHTML(),
        before,
    );
    assert.equal(
        await page
            .locator('[aria-label="Screen presentation"]')
            .getAttribute("data-seq"),
        "1",
    );
    assert.equal(
        await page
            .getByRole("button", { name: "Settings", exact: true })
            .evaluate((node) => node === document.activeElement),
        true,
    );
    assert.deepEqual(await calls(page), [
        {
            url: "http://127.0.0.1:4173/settings/v2?token=synthetic-screen-pair&include_muse=true",
            method: "GET",
            headers: undefined,
            body: undefined,
        },
        {
            url: "http://127.0.0.1:4173/settings/v1?token=synthetic-screen-pair",
            method: "GET",
            headers: undefined,
            body: undefined,
        },
    ]);
    await response(page, snapshot);
    await page.keyboard.press("Enter");
    await emit(page, frame(2, states[6]));
    assert.equal(
        await page.getByText("Microphone muted", { exact: true }).isVisible(),
        true,
    );
    await emit(page, frame(1));
    await page.keyboard.press("Escape");
    assert.equal(await page.locator("h1:visible").textContent(), "muted");
    assert.equal(
        await page
            .locator('[aria-label="Screen presentation"]')
            .getAttribute("data-seq"),
        "2",
    );
});

test("settings: a save result fades after 4 s, and Back cancels the fade", async (t) => {
    const page = await settingsPage(t);
    const status = page.locator(".settings-panel [role=status]");
    const timers = () => page.evaluate(() => window.timers.size);
    await page.getByLabel("Tool calls per task").fill("9");
    await response(page, { version: 1, restart_pending: false });
    const save = page.getByRole("button", { name: "Save settings" });
    await save.click();
    const saved = "Saved. Changes apply to the next conversation.";
    assert.equal(await status.textContent(), saved);
    await page.clock.runFor(3999);
    assert.equal(await status.textContent(), saved);
    await page.clock.runFor(1);
    assert.equal(await status.textContent(), "");
    await page.getByLabel("Tool calls per task").fill("10");
    await save.click();
    assert.equal(await status.textContent(), saved);
    const pending = await timers();
    await page.getByRole("button", { name: "Back", exact: true }).click();
    assert.equal(await timers(), pending - 1);
    assert.equal(await status.textContent(), "");
});

test("settings: the Live voice arrow is drawn like the stepper buttons", async (t) => {
    const page = await settingsPage(t);
    const voice = page.getByLabel("Live voice");
    const look = await voice.evaluate((select) => {
        const style = getComputedStyle(select);
        const plus = select
            .closest(".settings-fields")
            .querySelector(".settings-stepper button");
        return {
            appearance: style.appearance,
            paddingRight: style.paddingRight,
            background: style.backgroundImage,
            height: select.getBoundingClientRect().height,
            stepper: plus.getBoundingClientRect().height,
            stepperWidth: plus.getBoundingClientRect().width,
        };
    });
    assert.equal(look.appearance, "none", "no native arrow");
    assert.equal(look.paddingRight, "84px");
    assert.match(look.background, /^url\("data:image\/svg\+xml/);
    assert.equal(look.height, look.stepper);
    assert.equal(look.stepperWidth, 72);
});

test("settings: sparse saves preserve decimal caps and report exact validation without automatic retry", async (t) => {
    const page = await settingsPage(t);
    await page
        .getByRole("button", { name: "Save settings", exact: true })
        .click();
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        "No settings changed",
    );
    assert.equal((await calls(page)).length, 2);
    await page.getByLabel("Follow-up listening", { exact: true }).uncheck();
    await response(page, { version: 1, restart_pending: true });
    await page
        .getByRole("button", { name: "Save settings", exact: true })
        .click();
    assert.match(
        await page.locator(".settings-panel [role=status]").textContent(),
        /^Saved\./,
    );
    assert.equal(
        (await calls(page))[2].body,
        '{"version":1,"settings":{"voice":{"session_listen_enabled":false}}}',
    );
    await page
        .getByRole("button", { name: "Save settings", exact: true })
        .click();
    assert.equal((await calls(page)).length, 3);
    await page.getByLabel("Follow-up window (seconds)").fill("20");
    await page.getByLabel("Live voice").selectOption("cedar");
    await page.getByLabel("Daily request cap").fill("9007199254740993");
    await page.getByLabel("Tool calls per task").fill("10");
    const message = "agent.daily_request_cap: outside allowed range";
    await response(page, { version: 1, message }, 422);
    await page
        .getByRole("button", { name: "Save settings", exact: true })
        .click();
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        message,
    );
    assert.deepEqual(JSON.parse((await calls(page))[3].body), {
        version: 1,
        settings: {
            voice: { session_listen_seconds: 20 },
            codex: { voice: "cedar" },
            agent: {
                daily_request_cap: "9007199254740993",
                max_tool_calls: 10,
            },
        },
    });
    assert.equal(
        await page.getByLabel("Daily request cap").inputValue(),
        "9007199254740993",
    );
    await page.clock.runFor(10000);
    assert.equal((await calls(page)).length, 4);
    await response(page, { version: 1, restart_pending: false });
    await page
        .getByRole("button", { name: "Save settings", exact: true })
        .click();
    assert.equal((await calls(page)).length, 5);
    assert.match(
        await page.locator(".settings-panel [role=status]").textContent(),
        /next conversation/,
    );
});

test("settings: prompt text remains inert and exact; blank and over-32KiB values are refused locally", async (t) => {
    const page = await settingsPage(t);
    const text = " <img src=x onerror=alert(1)>\né🌈\n";
    for (const kind of ["voice", "runner"]) {
        await page.getByLabel(`${kind}.md`, { exact: true }).fill(text);
        await response(page, { version: 1, restart_pending: false });
        await page
            .getByRole("button", { name: `Save ${kind}.md`, exact: true })
            .click();
        assert.equal(
            await page.getByLabel(`${kind}.md`, { exact: true }).inputValue(),
            text,
        );
        const call = (await calls(page)).at(-1);
        assert.equal(
            call.url,
            `http://127.0.0.1:4173/settings/v1/prompts/${kind}?token=synthetic-screen-pair`,
        );
        assert.deepEqual(JSON.parse(call.body), { version: 1, text });
    }
    assert.equal(await page.locator("img").count(), 0);
    const count = (await calls(page)).length;
    for (const text of ["  \n", "é".repeat(16385)]) {
        await page.getByLabel("voice.md", { exact: true }).fill(text);
        await page
            .getByRole("button", { name: "Save voice.md", exact: true })
            .click();
        assert.equal((await calls(page)).length, count);
        assert.equal(
            await page
                .getByLabel("voice.md", { exact: true })
                .evaluate((node) => node.validationMessage),
            "Use nonblank text of at most 32 KiB.",
        );
    }
    await page.getByLabel("voice.md", { exact: true }).fill("é".repeat(16384));
    await page
        .getByRole("button", { name: "Save voice.md", exact: true })
        .click();
    assert.equal((await calls(page)).length, count + 1);
    await response(page, undefined, 204);
    await page.getByRole("button", { name: "Relink", exact: true }).click();
    assert.equal((await calls(page)).at(-1).body, '{"action":"relink"}');
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        "Port OS is already linked. Nothing changed.",
    );
});

test("settings: pairing, unavailable, malformed and persistence errors retain editable values", async (t) => {
    const page = await settingsPage(t);
    await page.getByLabel("voice.md", { exact: true }).fill("Keep this edit");
    for (const [status, body, expected] of [
        [401, undefined, "Pairing is unavailable. Reopen the paired screen."],
        [403, undefined, "Pairing is unavailable. Reopen the paired screen."],
        [
            503,
            undefined,
            "Settings are unavailable. Reopen Settings to try again.",
        ],
        [
            500,
            { version: 1, message: "voice.md: persistence failed" },
            "voice.md: persistence failed",
        ],
        [422, { version: 2, message: "bad" }, "Settings request was refused"],
        [400, undefined, "Settings request was refused"],
        [
            200,
            { version: 2, restart_pending: false },
            "Save response could not be read",
        ],
        [200, null, "Settings response could not be read"],
    ]) {
        await response(page, body, status);
        await page
            .getByRole("button", { name: "Save voice.md", exact: true })
            .click();
        assert.equal(
            await page.locator(".settings-panel [role=status]").textContent(),
            expected,
        );
        assert.equal(
            await page.getByLabel("voice.md", { exact: true }).inputValue(),
            "Keep this edit",
        );
    }
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await response(page, { ...snapshot, allowed_voices: [] });
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        "Settings response could not be read",
    );
    assert.equal(await page.locator(".settings-fields input").count(), 0);
});

test("settings: cancellation and unload never render a late response or resubmit a save", async (t) => {
    const page = await settingsPage(t);
    await response(page, undefined, "pending");
    await page.getByLabel("Daily request cap").fill("10");
    await page
        .getByRole("button", { name: "Save settings", exact: true })
        .click();
    assert.equal(
        await page
            .getByRole("button", { name: "Save settings", exact: true })
            .isDisabled(),
        true,
    );
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await page.evaluate(() =>
        window.resolvePost({
            status: 200,
            json: async () => ({ version: 1, restart_pending: true }),
        }),
    );
    assert.equal(
        await page.locator('[aria-label="Screen presentation"]').isVisible(),
        true,
    );
    assert.equal(await page.evaluate(() => window.timers.size), 1);
    await response(page, undefined, "pending");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
    assert.equal(await page.locator(".settings-panel").count(), 0);
    assert.equal(await page.evaluate(() => window.streams[0].closed), true);
    assert.equal(await page.evaluate(() => window.timers.size), 0);
    assert.equal((await calls(page)).length, 5);
});

test("settings: timed-out save stays uncertain without retry, and reopening reloads accepted values", async (t) => {
    const page = await settingsPage(t);
    await response(page, undefined, "pending");
    await page.getByLabel("Tool calls per task").fill("12");
    await page
        .getByRole("button", { name: "Save settings", exact: true })
        .click();
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        "Saving…",
    );
    await page.clock.runFor(10000);
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        "Request could not be confirmed. Reopen Settings to check before saving again.",
    );
    assert.equal(
        await page.getByLabel("Tool calls per task").inputValue(),
        "12",
    );
    assert.equal(
        await page
            .getByRole("button", { name: "Save settings", exact: true })
            .isEnabled(),
        true,
    );
    await page.clock.runFor(20000);
    assert.equal((await calls(page)).length, 3);
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await response(page, { ...snapshot, restart_pending: true });
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    assert.equal(
        await page.getByLabel("Tool calls per task").inputValue(),
        "8",
    );
    assert.equal(
        await page.locator(".settings-panel [role=status]").textContent(),
        "Saved settings are waiting for a bridge restart.",
    );
    assert.equal((await calls(page)).length, 5);
});

test("settings: invalid pairing bootstrap remains local and the state stream still updates", async (t) => {
    for (const actions of [
        "/actions",
        "/actions?token=",
        "/actions?token=a&token=b",
    ]) {
        const page = await pageFor(
            t,
            `?events=/events&actions=${encodeURIComponent(actions)}`,
            true,
        );
        await page
            .getByRole("button", { name: "Settings", exact: true })
            .click();
        assert.equal(
            await page.locator(".settings-panel [role=status]").textContent(),
            "Settings configuration is invalid",
        );
        assert.deepEqual(await calls(page), []);
        await emit(page, frame(1));
        await page.getByRole("button", { name: "Back", exact: true }).click();
        assert.equal(
            await page.locator("[data-ui-state=ready]:visible").count(),
            1,
        );
    }
});

test("settings: both supported viewports keep Back/status visible and forms scrollable with 64px targets", async (t) => {
    const page = await settingsPage(t);
    for (const height of [800, 774]) {
        await page.setViewportSize({ width: 1280, height });
        for (const name of ["Follow-up listening", "voice.md", "runner.md"]) {
            const control = page.getByLabel(name, { exact: true });
            await control.scrollIntoViewIfNeeded();
            const box = await control.boundingBox();
            assert.ok(box.x >= 0 && box.x + box.width <= 1280);
            assert.ok(box.height >= 64);
        }
        await page
            .getByRole("button", { name: "Relink", exact: true })
            .scrollIntoViewIfNeeded();
        for (const locator of [
            page.getByRole("button", { name: "Back", exact: true }),
            page.locator(".settings-panel [role=status]"),
        ]) {
            const box = await locator.boundingBox();
            assert.ok(box.y >= 0 && box.y + box.height <= height);
        }
        assert.deepEqual(
            await page.evaluate(() => ({
                width: document.documentElement.scrollWidth,
                height: document.documentElement.scrollHeight,
            })),
            { width: 1280, height },
        );
        await page.screenshot({
            path: `test-results/screenshots/settings-${height}.png`,
        });
    }
});
