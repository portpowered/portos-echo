import assert from "node:assert/strict";
import { cp, mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";

const id = "template@0.2.0";
const width = 960;
const height = 600;

function intersects(a, b) {
    return (
        a.x < b.x + b.width &&
        b.x < a.x + a.width &&
        a.y < b.y + b.height &&
        b.y < a.y + a.height
    );
}

async function observe(state, surfaces, violations) {
    const boxes = [];
    for (const [name, locator] of surfaces) {
        await locator.waitFor({ state: "visible" });
        const box = await locator.boundingBox();
        assert.ok(box, `${state} ${name} has a visible rectangle`);
        if (
            box.x < 0 ||
            box.y < 0 ||
            box.x + box.width > width ||
            box.y + box.height > height
        )
            violations.push(`${state}: ${name} leaves the viewport`);
        for (const [other, prior] of boxes) {
            if (intersects(box, prior))
                violations.push(`${state}: ${name} overlaps ${other}`);
        }
        boxes.push([name, box]);
    }
    console.log(
        `FACE 960×600 ${state}: ${JSON.stringify(Object.fromEntries(boxes))}`,
    );
}

export async function checkTemplateLayout(
    t,
    { open, playing, reloads, refresh },
) {
    const generated = process.env.SCREEN_PACK_EYES_FIXTURE;
    assert.ok(generated, "suite owner must prepare template@0.2.0");
    const fixture = await open(t, {
        locale: "en-GB",
        packArchive: join(generated, "template-0.2.0.screenpack.tar.gz"),
        initialState: {
            state: "link_code",
            user_code: "DEMO-CODE",
            verification_uri: "https://example.invalid/verify",
            expires_at_unix: 1700000000,
        },
        async prepare(home, older) {
            const screens = join(home, "screens");
            await mkdir(screens);
            const legacy = "template@0.1.0";
            await cp(join(older, legacy), join(screens, legacy), {
                recursive: true,
            });
        },
    });
    const { page } = fixture;
    await page.setViewportSize({ width, height });
    assert.equal(
        (await fixture.api("/screen-packs/v1")).active,
        "template@0.1.0",
    );
    const settings = page.getByRole("button", {
        name: "Settings",
        exact: true,
    });
    await settings.click();
    await refresh(page);
    await page
        .getByLabel("Pack HTTPS URL", { exact: true })
        .fill(fixture.peer.url);
    await page.getByLabel("Pack SHA-256", { exact: true }).fill(fixture.sha256);
    await page
        .getByRole("button", { name: "Download pack", exact: true })
        .click();
    await reloads(page, 1);
    assert.equal((await fixture.api("/screen-packs/v1")).active, id);
    for (const file of ["pack.json", "rest.png"]) {
        assert.deepEqual(
            await readFile(join(fixture.home, "screens", id, file)),
            await readFile(join(generated, id, file)),
        );
    }
    console.log(`FACE installed ${id} archive SHA-256: ${fixture.sha256}`);
    await page.getByRole("button", { name: "Back", exact: true }).click();
    // Both versions have a setup.webm. Wait until the installed pack owns the stage.
    await page.waitForFunction((identity) => {
        const rest = document.querySelector(".pack-stage img");
        return (
            rest &&
            decodeURIComponent(new URL(rest.src).pathname).endsWith(
                `/${identity}/rest.png`,
            )
        );
    }, id);
    await playing(page, "setup.webm");
    await page.getByText("DEMO-CODE", { exact: true }).waitFor();

    const stage = page.locator(".pack-stage");
    const heading = page.locator(".pack-status");
    const violations = [];
    const stageBox = await stage.boundingBox();
    assert.ok(stageBox);
    if (stageBox.width * stageBox.height <= (width * height) / 2)
        violations.push("stage covers half the viewport or less");
    assert.deepEqual(
        await stage.evaluate((el) => [
            getComputedStyle(el).backgroundColor,
            getComputedStyle(document.documentElement).backgroundColor,
        ]),
        ["rgb(16, 24, 39)", "rgb(16, 24, 39)"],
    );

    await observe(
        "Link Code",
        [
            ["stage", stage],
            ["status", heading],
            ["settings", settings],
            ["link code slot", page.locator(".pack-link-code")],
        ],
        violations,
    );

    const cases = [
        {
            clip: "approval.webm",
            slot: ".pack-approval",
            text: "Allow the requested action for this synthetic fixture? The request will write its confirmation and finish the current turn. Check the requested settings and verify that this is the outcome you intend before you continue.",
            controls: ["Allow", "Deny"],
            frame: {
                state: "approval",
                id: "approval-layout",
                accept_label: "Allow",
                deny_label: "Deny",
            },
        },
        {
            clip: "error.webm",
            slot: ".pack-error",
            text: "The bridge could not complete the last request. Check the settings and try again.",
            controls: ["Relink", "Dismiss"],
            frame: { state: "error", relink: true },
        },
    ];
    await mkdir("test-results/screenshots", { recursive: true });
    for (const { clip, slot, text, controls, frame } of cases) {
        await fixture.update({
            ...frame,
            ...(frame.state === "approval"
                ? { prompt: text }
                : { message: text }),
        });
        await playing(page, clip);
        const card = page.locator(slot);
        const message = card.getByText(text, { exact: true });
        const buttons = controls.map((name) => [
            name,
            card.getByRole("button", { name, exact: true }),
        ]);
        await observe(
            frame.state,
            [
                ["stage", stage],
                ["status", heading],
                ["settings", settings],
                ["context slot", card],
            ],
            violations,
        );
        if (await message.evaluate((el) => el.scrollHeight > el.clientHeight))
            violations.push(`${frame.state}: reviewed prompt does not fit`);
        for (const [, button] of buttons) await button.click({ trial: true });
        await page.screenshot({
            path: `test-results/screenshots/template-960x600-${frame.state}.png`,
        });
        // The text and buttons share the card, and each must also clear the
        // other native controls and the stage.
        await observe(
            frame.state,
            [
                ["stage", stage],
                ["status", heading],
                ["message", message],
                ...buttons,
            ],
            violations,
        );
    }
    const longPrompt = cases[0].text.repeat(5);
    await fixture.update({
        ...cases[0].frame,
        id: "scroll",
        prompt: longPrompt,
    });
    await playing(page, "approval.webm");
    const longCard = page.locator(".pack-approval");
    const longMessage = longCard.getByText(longPrompt, { exact: true });
    assert.equal(
        await longMessage.evaluate((el) => el.scrollHeight > el.clientHeight),
        true,
    );
    const longButtons = ["Allow", "Deny"].map((name) => [
        name,
        longCard.getByRole("button", { name, exact: true }),
    ]);
    await observe(
        "Long Approval",
        [
            ["stage", stage],
            ["status", heading],
            ["message", longMessage],
            ...longButtons,
        ],
        violations,
    );
    for (const [, button] of longButtons) await button.click({ trial: true });
    await fixture.update({ state: "connected" });
    await playing(page, "idle.webm");
    await observe(
        "Connected",
        [
            ["stage", stage],
            ["settings", settings],
            ["clock", page.locator(".pack-clock")],
            ["date", page.locator(".pack-date")],
        ],
        violations,
    );
    await fixture.update({ state: "ready", message: "Ready" });
    await playing(page, "idle.webm");
    const begin = page.getByRole("button", { name: "Begin session" });
    await begin.press("Enter");
    assert.deepEqual(await fixture.next(), {
        action: { action: "tap", target: "primary" },
    });
    // An accepted action draws no status box over the face.
    await page.locator("button.home-tap:enabled").waitFor();
    const feedback = page.locator('[role="status"]');
    assert.equal(await feedback.textContent(), "");
    assert.equal(await feedback.isVisible(), false);
    await observe(
        "Ready after activation",
        [
            ["stage", stage],
            ["settings", settings],
            ["clock", page.locator(".pack-clock")],
            ["date", page.locator(".pack-date")],
        ],
        violations,
    );
    await page.screenshot({
        path: "test-results/screenshots/template-960x600-ready-feedback.png",
    });
    assert.deepEqual(fixture.outside, []);
    assert.deepEqual(violations, []);
}
