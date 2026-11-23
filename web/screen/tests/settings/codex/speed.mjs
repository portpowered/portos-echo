import { test } from "node:test";
import assert from "node:assert/strict";
import { response } from "../page.mjs";
import { calls, open, section, v2Snapshot } from "../page-v2.mjs";

test("settings V2 keeps an explicit standard-speed edit after a failed save", async (t) => {
    const data = v2Snapshot();
    data.settings.codex.delegate_speed = "fast";
    const page = await open(t, data, 200, {
        context: { viewport: { width: 960, height: 600 } },
    });
    await section(page, "OpenAI").tap();
    const fast = page.getByRole("combobox", { name: "Delegated speed" });
    assert.equal(await fast.inputValue(), "fast");
    await fast.selectOption("standard");
    assert.equal(await fast.inputValue(), "standard");
    await fast.scrollIntoViewIfNeeded();
    const box = await fast.boundingBox();
    assert.ok(box.width >= 64 && box.height >= 64 && box.y + box.height <= 600);
    await page.screenshot({
        path: "test-results/screenshots/settings-fast-off-960x600.png",
    });
    const save = page.getByRole("button", {
        name: "Save settings",
        exact: true,
    });
    const status = page.getByRole("status");
    const error = "Could not save file; previous file retained";
    await response(page, { version: 2, message: error }, 500);
    await save.tap();
    assert.equal(await status.textContent(), error);
    assert.equal(await fast.inputValue(), "standard");
    await response(page, { version: 2, restart_pending: true });
    await save.tap();
    assert.equal(
        await status.textContent(),
        "Saved. The bridge will restart after the current reply finishes.",
    );
    assert.deepEqual(
        (await calls(page))
            .filter(({ method }) => method === "PATCH")
            .map(({ body }) => JSON.parse(body)),
        Array.from({ length: 2 }, () => ({
            version: 2,
            settings: { codex: { delegate_speed: "standard" } },
        })),
    );
});
