import { test } from "node:test";
import assert from "node:assert/strict";
import { pageFor, emit, calls } from "../browser.mjs";
import { frame, states } from "../fixtures.mjs";
import { response, snapshot } from "./page.mjs";

// The Echo's 1280x800 panel at density 213 gives the WebView 960x600 CSS px.
const ECHO = { width: 960, height: 600 };
const query =
    "?events=%2Fevents%3Ftoken%3Dsynthetic-screen-pair&actions=%2Factions%3Ftoken%3Dsynthetic-screen-pair";
const relinkError = states[10];
const status = (page) =>
    page.locator(".settings-panel [role=status]").textContent();
const key = (page, token) =>
    page.locator(`.settings-keyboard button[data-key="${token}"]`);
const row = (page, label) =>
    page.locator(".settings-row").filter({ hasText: label });
const step = (page, label, name) =>
    row(page, label).getByRole("button", { name, exact: true });

async function open(t, changes = {}) {
    const page = await pageFor(t, query, true);
    await page.setViewportSize(ECHO);
    await emit(page, frame(1));
    const gear = await page
        .getByRole("button", { name: "Settings", exact: true })
        .boundingBox();
    await response(page, { ...snapshot, ...changes });
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    assert.equal(await status(page), "Settings loaded");
    return { page, gear };
}

test("settings usability: at 960x600 Back sits in the gear's top-right corner and the fields take 78% of the height", async (t) => {
    const { page, gear } = await open(t);
    const back = await page
        .getByRole("button", { name: "Back", exact: true })
        .boundingBox();
    assert.deepEqual(back, { x: 816, y: 12, width: 120, height: 64 });
    // The top-right quarter, ending exactly where the gear ends.
    assert.ok(back.x >= ECHO.width / 2);
    assert.ok(back.y + back.height <= ECHO.height / 2);
    assert.equal(back.x + back.width, gear.x + gear.width);
    assert.deepEqual(gear, { x: 864, y: 24, width: 72, height: 72 });
    const title = await page.locator(".settings-panel h1").boundingBox();
    assert.equal(title.x, 24);
    assert.ok(title.y >= back.y && title.y + title.height <= 76);
    assert.ok(title.x + title.width <= back.x);
    const fields = await page.locator(".settings-fields").boundingBox();
    assert.deepEqual(fields, { x: 24, y: 124, width: 912, height: 468 });
    assert.ok(fields.height >= 0.7 * ECHO.height);
    // Label at the left, control at the right, on one line.
    for (const name of [
        "Follow-up listening",
        "Follow-up window (seconds)",
        "Live voice",
        "Daily request cap",
        "Tool calls per task",
    ]) {
        const control = page.getByLabel(name, { exact: true });
        await control.scrollIntoViewIfNeeded();
        const box = await control.boundingBox();
        const text = await page
            .locator("label")
            .filter({ hasText: name })
            .boundingBox();
        assert.equal(text.x, 32, name);
        assert.ok(text.x + text.width <= box.x, name);
        assert.ok(
            text.y + text.height / 2 > box.y &&
                text.y + text.height / 2 < box.y + box.height,
            name,
        );
        assert.ok(box.x + box.width <= fields.x + fields.width, name);
        assert.ok(box.height >= 64, name);
    }
    for (const button of await page
        .locator(".settings-panel button:visible")
        .all()) {
        const box = await button.boundingBox();
        assert.ok(box.width >= 56 && box.height >= 56, JSON.stringify(box));
    }
    await page.screenshot({
        path: "test-results/screenshots/settings-960x600.png",
    });
    // The keyboard takes its room from the fields; it never covers the field.
    const prompt = page.getByLabel("voice.md", { exact: true });
    await prompt.click();
    const board = await page.locator(".settings-keyboard").boundingBox();
    assert.deepEqual(board, { x: 24, y: 342, width: 912, height: 250 });
    const shrunk = await page.locator(".settings-fields").boundingBox();
    assert.equal(shrunk.y + shrunk.height, board.y);
    const focused = await prompt.boundingBox();
    assert.ok(focused.y >= shrunk.y, JSON.stringify(focused));
    assert.ok(focused.y + focused.height <= board.y, JSON.stringify(focused));
    const keys = await page.locator(".settings-keyboard button").all();
    assert.equal(keys.length, 34);
    for (const button of keys) {
        const box = await button.boundingBox();
        assert.equal(box.height, 56);
        assert.ok(box.width >= 56, JSON.stringify(box));
    }
    assert.deepEqual(
        await page.evaluate(() => ({
            width: document.documentElement.scrollWidth,
            height: document.documentElement.scrollHeight,
        })),
        ECHO,
    );
    await page.screenshot({
        path: "test-results/screenshots/settings-960x600-keyboard.png",
    });
    await page.getByRole("button", { name: "Back", exact: true }).click();
    assert.equal(
        await page.evaluate(() =>
            document.querySelector("main").classList.contains("settings-open"),
        ),
        false,
    );
    assert.equal(await page.locator(".settings-keyboard").isVisible(), false);
});

test("settings usability: steppers clamp to each field's range, repeat while held and build the exact patch", async (t) => {
    const { page } = await open(t);
    const tools = page.getByLabel("Tool calls per task", { exact: true });
    const seconds = page.getByLabel("Follow-up window (seconds)", {
        exact: true,
    });
    const cap = page.getByLabel("Daily request cap", { exact: true });
    await step(page, "Tool calls per task", "Increase").click();
    assert.equal(await tools.inputValue(), "9");
    await step(page, "Tool calls per task", "Decrease").click({
        clickCount: 3,
    });
    assert.equal(await tools.inputValue(), "6");
    for (let press = 0; press < 8; press++)
        await step(page, "Tool calls per task", "Decrease").click();
    assert.equal(await tools.inputValue(), "1");
    await tools.fill("31");
    await step(page, "Tool calls per task", "Increase").click({
        clickCount: 3,
    });
    assert.equal(await tools.inputValue(), "32");
    // The cap is already the largest value the bridge stores.
    await step(page, "Daily request cap", "Increase").click();
    assert.equal(await cap.inputValue(), "9223372036854775807");
    await step(page, "Daily request cap", "Decrease").click();
    assert.equal(await cap.inputValue(), "9223372036854775806");
    await cap.fill("1");
    await step(page, "Daily request cap", "Decrease").click();
    await step(page, "Daily request cap", "Decrease").click();
    assert.equal(await cap.inputValue(), "0");
    await cap.fill("");
    await step(page, "Daily request cap", "Increase").click();
    assert.equal(await cap.inputValue(), "0");
    await step(page, "Daily request cap", "Increase").click({ clickCount: 2 });
    assert.equal(await cap.inputValue(), "2");
    // Holding: nothing for 500 ms, then one step every 100 ms; ten single
    // steps, then tens. Releasing adds no extra step.
    await seconds.scrollIntoViewIfNeeded();
    const plus = await step(
        page,
        "Follow-up window (seconds)",
        "Increase",
    ).boundingBox();
    await page.mouse.move(plus.x + plus.width / 2, plus.y + plus.height / 2);
    await page.mouse.down();
    await page.clock.runFor(499);
    assert.equal(await seconds.inputValue(), "15");
    await page.clock.runFor(1);
    assert.equal(await seconds.inputValue(), "16");
    await page.clock.runFor(900);
    assert.equal(await seconds.inputValue(), "25");
    await page.clock.runFor(300);
    assert.equal(await seconds.inputValue(), "55");
    await page.mouse.up();
    await page.clock.runFor(5000);
    assert.equal(await seconds.inputValue(), "55");
    assert.equal(await page.evaluate(() => window.timers.size), 0);
    await seconds.fill("295");
    await step(page, "Follow-up window (seconds)", "Increase").click({
        clickCount: 7,
    });
    assert.equal(await seconds.inputValue(), "300");
    await seconds.fill("5");
    await step(page, "Follow-up window (seconds)", "Decrease").click({
        clickCount: 4,
    });
    assert.equal(await seconds.inputValue(), "3");
    await response(page, { version: 1, restart_pending: false });
    await page
        .getByRole("button", { name: "Save settings", exact: true })
        .click();
    assert.equal(
        (await calls(page)).at(-1).body,
        '{"version":1,"settings":{"voice":{"session_listen_seconds":3},"agent":{"daily_request_cap":"2","max_tool_calls":32}}}',
    );
    assert.equal(
        await status(page),
        "Saved. Changes apply to the next conversation.",
    );
});

test("settings usability: the on-screen keyboard types Hi! at the caret and the prompt save sends exactly that", async (t) => {
    const { page } = await open(t);
    const prompt = page.getByLabel("voice.md", { exact: true });
    const board = page.locator(".settings-keyboard");
    assert.equal(await board.isVisible(), false);
    assert.equal(await prompt.getAttribute("inputmode"), null);
    await prompt.click();
    assert.equal(await board.isVisible(), true);
    await prompt.evaluate((node) => {
        window.inputs = [];
        node.addEventListener("input", (event) =>
            window.inputs.push([event.inputType, event.data]),
        );
        node.select();
    });
    await key(page, "back").click();
    assert.equal(await prompt.inputValue(), "");
    await key(page, "back").click();
    await key(page, "space").click();
    // A blank prompt is refused; typing must clear that refusal.
    await page
        .getByRole("button", { name: "Save voice.md", exact: true })
        .click();
    assert.equal(
        await prompt.evaluate((node) => node.validationMessage),
        "Use nonblank text of at most 32 KiB.",
    );
    // The refused field takes focus, so the keyboard is there to fix it.
    assert.equal(await board.isVisible(), true);
    await key(page, "back").click();
    assert.equal(
        await key(page, "shift").getAttribute("aria-pressed"),
        "false",
    );
    await key(page, "shift").click();
    assert.equal(await key(page, "shift").getAttribute("aria-pressed"), "true");
    await key(page, "H").click();
    assert.equal(await prompt.evaluate((node) => node.validationMessage), "");
    // Shift lasts one letter.
    assert.equal(await key(page, "H").count(), 0);
    await key(page, "i").click();
    await key(page, "layer").click();
    await key(page, "!").click();
    assert.equal(await prompt.inputValue(), "Hi!");
    assert.equal(
        await prompt.evaluate((node) => node === document.activeElement),
        true,
    );
    assert.deepEqual(await page.evaluate(() => window.inputs), [
        ["deleteContentBackward", null],
        ["insertText", " "],
        ["deleteContentBackward", null],
        ["insertText", "H"],
        ["insertText", "i"],
        ["insertText", "!"],
    ]);
    // Insert and delete at the caret, a newline, a space and a wide character.
    await prompt.evaluate((node) => node.setSelectionRange(1, 1));
    await key(page, "shift").click();
    await key(page, "€").click();
    await key(page, "enter").click();
    await key(page, "layer").click();
    await key(page, "space").click();
    assert.equal(await prompt.inputValue(), "H€\n i!");
    for (let press = 0; press < 3; press++) await key(page, "back").click();
    assert.equal(await prompt.inputValue(), "Hi!");
    await prompt.evaluate((node) => {
        node.value = "Hi!🌈";
        node.setSelectionRange(5, 5);
    });
    // Holding Del repeats; one press removes one whole character.
    const del = await key(page, "back").boundingBox();
    await page.mouse.move(del.x + del.width / 2, del.y + del.height / 2);
    await page.mouse.down();
    await page.clock.runFor(500);
    assert.equal(await prompt.inputValue(), "Hi!");
    await page.mouse.up();
    assert.equal(await prompt.inputValue(), "Hi!");
    await key(page, "done").click();
    assert.equal(await board.isVisible(), false);
    await response(page, { version: 1, restart_pending: false });
    await page
        .getByRole("button", { name: "Save voice.md", exact: true })
        .click();
    const call = (await calls(page)).at(-1);
    assert.equal(
        call.url,
        "http://127.0.0.1:4173/settings/v1/prompts/voice?token=synthetic-screen-pair",
    );
    assert.equal(call.body, '{"version":1,"text":"Hi!"}');
    // A numeric text field opens on digits; Enter there puts the keyboard away.
    const cap = page.getByLabel("Daily request cap", { exact: true });
    await cap.click();
    await cap.evaluate((node) => node.select());
    await key(page, "4").click();
    await key(page, "2").click();
    assert.equal(await cap.inputValue(), "42");
    await key(page, "enter").click();
    assert.equal(await board.isVisible(), false);
    // A tap on the field reopens it; a tap elsewhere in the panel closes it.
    await cap.click();
    assert.equal(await board.isVisible(), true);
    await page.locator(".settings-panel h1").click();
    assert.equal(await board.isVisible(), false);
    await key(page, "4").evaluate((node) => node.click());
    assert.equal(await cap.inputValue(), "42");
});

test("settings usability: Relink says what happens for each link status and never claims instructions that are not shown", async (t) => {
    const fromPc =
        "Link it from the PC with echo-bridge link. Linking on this screen is coming.";
    for (const [link_status, shown, expected] of [
        [
            "unconfigured",
            "Port OS: unconfigured",
            `Port OS is not linked yet. ${fromPc}`,
        ],
        [
            "link_required",
            "Port OS: link required",
            `Port OS needs linking again. ${fromPc}`,
        ],
        [
            "linked",
            "Port OS: linked",
            "Port OS is already linked. Nothing changed.",
        ],
    ]) {
        const { page } = await open(t, { link_status });
        assert.equal(await page.getByText(shown, { exact: true }).count(), 1);
        // An error frame nobody asked for leaves the panel's status alone.
        await emit(page, frame(2, relinkError));
        assert.equal(await status(page), "Settings loaded");
        await response(page, undefined, 204);
        await page.getByRole("button", { name: "Relink", exact: true }).click();
        assert.deepEqual((await calls(page)).slice(1), [
            {
                url: "http://127.0.0.1:4173/actions?token=synthetic-screen-pair",
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: '{"action":"relink"}',
            },
        ]);
        assert.equal(await status(page), expected);
        assert.equal(
            await page
                .getByRole("button", { name: "Relink", exact: true })
                .isEnabled(),
            true,
        );
        // A frame that is not the relink error changes nothing.
        await emit(page, frame(3, states[11]));
        assert.equal(await status(page), expected);
        // The bridge republishes its relink error: instructions are on screen.
        await emit(page, frame(4, relinkError));
        assert.equal(
            await status(page),
            "Relink requested. Return to the screen for instructions.",
        );
    }
    // The republished frame can also arrive before the action is acknowledged.
    const { page } = await open(t, { link_status: "link_required" });
    await response(page, undefined, "pending");
    await page.getByRole("button", { name: "Relink", exact: true }).click();
    await emit(page, frame(2, relinkError));
    assert.equal(await status(page), "Loading…");
    await page.evaluate(() => window.resolvePost({ status: 204 }));
    assert.equal(
        await status(page),
        "Relink requested. Return to the screen for instructions.",
    );
});
