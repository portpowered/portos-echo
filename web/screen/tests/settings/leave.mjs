import { test } from "node:test";
import assert from "node:assert/strict";
import { calls } from "../browser.mjs";
import { response, settingsPage, snapshot } from "./page.mjs";
import { open, v2Snapshot } from "./page-v2.mjs";
import { base, empty, load, saveResponse, token } from "./pack-fixture.mjs";

const QUESTION = "Save your changes before leaving?";
const origin = "http://127.0.0.1:4173/settings/v1";
const pair = "?token=synthetic-screen-pair";
const saved = { version: 1, restart_pending: false };
const button = (page, name) => page.getByRole("button", { name, exact: true });
const dialog = (page) => page.getByRole("alertdialog", { name: QUESTION });
const panel = (page) => page.locator(".settings-panel");
const status = (page) => page.locator(".settings-panel > [role=status]");
const voice = (page) => page.getByLabel("voice.md", { exact: true });
const tools = (page) => page.getByLabel("Tool calls per task", { exact: true });
const focused = (control) =>
    control.evaluate((node) => node === document.activeElement);
const sent = async (page, from) =>
    (await calls(page)).slice(from).map(({ url, body }) => ({ url, body }));
const pack = (name) => ({
    id: `${name}@1.0.0`,
    name,
    version: "1.0.0",
    rest_url: null,
});
const packs = ["studio", "layout"].map(pack);

async function reopen(page) {
    await response(page, snapshot);
    await button(page, "Settings").click();
    await page.locator('.settings-fields[aria-busy="false"]').waitFor();
}
// Settings is still open, the question is gone and Back has the focus.
async function stayed(page) {
    assert.equal(await dialog(page).isVisible(), false);
    assert.equal(await panel(page).isVisible(), true);
    assert.equal(await focused(button(page, "Back")), true);
}
async function left(page) {
    assert.equal(await panel(page).isVisible(), false);
    assert.equal(await focused(button(page, "Settings")), true);
    assert.equal(await status(page).textContent(), "");
}
async function ask(page) {
    await button(page, "Back").click();
    assert.equal(await dialog(page).isVisible(), true);
    assert.equal(await focused(button(page, "Save")), true);
}

// The shared rule: the save button is the last item of a right-justified
// action row that spans its form, so it ends on the form's right edge.
async function rightAligned(page, name) {
    const save = button(page, name);
    await save.scrollIntoViewIfNeeded();
    const edges = await save.evaluate((node) => {
        const row = node.parentElement;
        const box = node.getBoundingClientRect();
        const area = row.getBoundingClientRect();
        const form = row.parentElement.getBoundingClientRect();
        return {
            rule: row.className,
            justify: getComputedStyle(row).justifyContent,
            last: row.lastElementChild === node,
            right: Math.abs(area.right - box.right) < 1,
            row:
                Math.abs(area.left - form.left) < 1 &&
                Math.abs(form.right - area.right) < 1,
            free: box.left - area.left,
            height: box.height,
        };
    });
    assert.deepEqual(
        { ...edges, free: edges.free > 100, height: edges.height >= 64 },
        {
            rule: "settings-actions",
            justify: "flex-end",
            last: true,
            right: true,
            row: true,
            free: true,
            height: true,
        },
        name,
    );
}

test("settings leave: every section's save button ends on the right edge of its row", async (t) => {
    const page = await settingsPage(t);
    await load(page, { ...empty, installed: packs, active: packs[0].id });
    for (const name of [
        "Save settings",
        "Use screen pack",
        "Save voice.md",
        "Save runner.md",
    ])
        await rightAligned(page, name);
    const v2 = await open(t, v2Snapshot());
    await v2.locator('.settings-fields[aria-busy="false"]').waitFor();
    await v2.evaluate(() => {
        for (const details of document.querySelectorAll(".settings-section"))
            details.open = true;
    });
    for (const name of ["Save settings", "Save Port OS permissions"])
        await rightAligned(v2, name);
});

test("settings leave: with no unsaved edit, Back and Escape close at once", async (t) => {
    const page = await settingsPage(t);
    await button(page, "Back").click();
    await left(page);
    await reopen(page);
    // An edit put back to its loaded value is not an edit.
    await voice(page).fill("Changed");
    await tools(page).fill("9");
    await voice(page).fill(snapshot.prompts.voice);
    await tools(page).fill("8");
    await page.keyboard.press("Escape");
    await left(page);
    // A saved edit is not unsaved either.
    await reopen(page);
    await voice(page).fill("Saved before leaving");
    await response(page, saved);
    await button(page, "Save voice.md").click();
    const count = (await calls(page)).length;
    await button(page, "Back").click();
    await left(page);
    assert.equal(await dialog(page).isVisible(), false);
    assert.equal((await calls(page)).length, count);
});

test("settings leave: an unsaved edit asks first; Cancel, Escape and a tap outside keep Settings and the edit", async (t) => {
    const page = await settingsPage(t);
    await voice(page).fill("Keep this edit");
    await ask(page);
    // The on-screen keyboard is put away, and the three answers are touch-sized.
    assert.equal(await page.locator(".settings-keyboard").isVisible(), false);
    assert.equal(await dialog(page).getAttribute("aria-modal"), "true");
    // Save is the rightmost answer, with the secondary answers to its left.
    let edge = 0;
    for (const name of ["Cancel", "Don't save", "Save"]) {
        const box = await button(page, name).boundingBox();
        assert.ok(box.width >= 64 && box.height >= 64, name);
        assert.ok(box.x >= edge, name);
        edge = box.x + box.width;
    }
    await page.screenshot({
        path: "test-results/screenshots/settings-leave-question.png",
    });
    // Focus stays on the answers.
    await page.keyboard.press("Tab");
    assert.equal(await focused(button(page, "Cancel")), true);
    await page.keyboard.press("Shift+Tab");
    assert.equal(await focused(button(page, "Save")), true);
    await page.keyboard.press("Shift+Tab");
    assert.equal(await focused(button(page, "Don't save")), true);
    await button(page, "Cancel").click();
    await stayed(page);
    await ask(page);
    await page.keyboard.press("Escape");
    await stayed(page);
    await ask(page);
    await page.mouse.click(12, 400);
    await stayed(page);
    // Enter on Cancel is Cancel, not Save.
    await ask(page);
    await button(page, "Cancel").press("Enter");
    await stayed(page);
    assert.equal(await voice(page).inputValue(), "Keep this edit");
    assert.equal((await calls(page)).length, 2);
});

test("settings leave: Don't save closes with no save request and drops the edit", async (t) => {
    const page = await settingsPage(t);
    await voice(page).fill("Dropped");
    await tools(page).fill("9");
    await ask(page);
    await button(page, "Don't save").click();
    await left(page);
    assert.equal((await calls(page)).length, 2);
    await reopen(page);
    assert.equal(await voice(page).inputValue(), snapshot.prompts.voice);
    assert.equal(await tools(page).inputValue(), "8");
    await button(page, "Back").click();
    await left(page);
    assert.equal((await calls(page)).length, 4);
});

test("settings leave: Save sends exactly each edited section's own request, then closes", async (t) => {
    const page = await settingsPage(t);
    await voice(page).fill("Only the voice prompt");
    await ask(page);
    await response(page, saved);
    await button(page, "Save").click();
    await left(page);
    assert.deepEqual(await sent(page, 2), [
        {
            url: `${origin}/prompts/voice${pair}`,
            body: '{"version":1,"text":"Only the voice prompt"}',
        },
    ]);
    // Enter is Save; two edited sections save in page order.
    await reopen(page);
    const from = (await calls(page)).length;
    await page.getByLabel("runner.md", { exact: true }).fill("Runner too");
    await tools(page).fill("9");
    await ask(page);
    await response(page, saved);
    await page.keyboard.press("Enter");
    await left(page);
    assert.deepEqual(await sent(page, from), [
        {
            url: origin + pair,
            body: '{"version":1,"settings":{"agent":{"max_tool_calls":9}}}',
        },
        {
            url: `${origin}/prompts/runner${pair}`,
            body: '{"version":1,"text":"Runner too"}',
        },
    ]);
});

test("settings leave: Save sends the edited Port OS permissions and screen pack through their own requests", async (t) => {
    const v2 = await open(t, v2Snapshot());
    await v2.locator('.settings-fields[aria-busy="false"]').waitFor();
    await v2.locator("summary", { hasText: "Port OS permissions" }).tap();
    await v2.getByText("message:send", { exact: true }).tap();
    await ask(v2);
    await response(v2, { version: 2, restart_pending: true });
    await button(v2, "Save").tap();
    await left(v2);
    assert.deepEqual(
        (await calls(v2))
            .filter(({ method }) => method !== "GET")
            .map(({ method, body }) => ({ method, body })),
        [
            {
                method: "PATCH",
                body: '{"version":2,"settings":{"portos":{"scopes":["endpoint:read"]}}}',
            },
        ],
    );
    const page = await settingsPage(t);
    await load(page, { ...empty, installed: packs, active: packs[0].id });
    const from = (await calls(page)).length;
    await page
        .getByLabel("Screen pack", { exact: true })
        .selectOption(packs[1].id);
    await ask(page);
    await saveResponse(page, 200, {
        ...empty,
        installed: packs,
        active: packs[1].id,
    });
    await button(page, "Save").click();
    await left(page);
    assert.deepEqual(await sent(page, from), [
        {
            url: base + "/selection" + token,
            body: `{"version":1,"screen_pack":"${packs[1].id}"}`,
        },
        { url: base + token, body: undefined },
    ]);
});

test("settings leave: a failed Save keeps Settings open with the error and the edits", async (t) => {
    const page = await settingsPage(t);
    await tools(page).fill("9");
    await voice(page).fill("Not saved yet");
    await ask(page);
    const message = "settings: persistence failed";
    await response(page, { version: 1, message }, 500);
    await button(page, "Save").click();
    await stayed(page);
    assert.equal(await status(page).textContent(), message);
    // The first refusal stops the run: the prompt was never sent.
    assert.deepEqual(await sent(page, 2), [
        {
            url: origin + pair,
            body: '{"version":1,"settings":{"agent":{"max_tool_calls":9}}}',
        },
    ]);
    assert.equal(await tools(page).inputValue(), "9");
    assert.equal(await voice(page).inputValue(), "Not saved yet");
    // A value the form refuses locally sends nothing and stays open too.
    await voice(page).fill("  ");
    await response(page, saved);
    await ask(page);
    await button(page, "Save").click();
    assert.equal(await panel(page).isVisible(), true);
    assert.equal(
        await voice(page).evaluate((node) => node.validationMessage),
        "Use nonblank text of at most 32 KiB.",
    );
    // The settings before it were saved on the way.
    assert.equal((await calls(page)).length, 4);
    // While another request is running, Save waits for the owner to retry.
    await voice(page).fill("Third try");
    await response(page, undefined, "pending");
    await button(page, "Save voice.md").click();
    await ask(page);
    await button(page, "Save").click();
    await stayed(page);
    assert.equal(
        await status(page).textContent(),
        "Another request is still running. Try again when it ends.",
    );
    assert.equal((await calls(page)).length, 5);
});
