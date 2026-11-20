import assert from "node:assert/strict";
import { cp, mkdir } from "node:fs/promises";
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

export async function checkTemplateLayout(t, open, playing) {
    const generated = process.env.SCREEN_PACK_EYES_FIXTURE;
    assert.ok(generated, "suite owner must prepare template@0.2.0");
    const fixture = await open(t, {
        locale: "en-GB",
        initialState: {
            state: "link_code",
            user_code: "DEMO-CODE",
            verification_uri: "https://example.invalid/verify",
            expires_at_unix: 1700000000,
        },
        async prepare(home) {
            const screens = join(home, "screens");
            await mkdir(screens);
            await cp(join(generated, id), join(screens, id), {
                recursive: true,
            });
        },
    });
    const { page } = fixture;
    await page.setViewportSize({ width, height });
    assert.equal((await fixture.api("/screen-packs/v1")).active, id);
    await playing(page, "setup.webm");
    await page.getByText("DEMO-CODE", { exact: true }).waitFor();

    const stage = page.locator(".pack-stage");
    const heading = page.locator(".pack-status");
    const violations = [];
    const stageBox = await stage.boundingBox();
    assert.ok(stageBox);
    if (stageBox.width * stageBox.height <= (width * height) / 2)
        violations.push("stage covers half the viewport or less");

    await observe(
        "Link Code",
        [
            ["stage", stage],
            ["status", heading],
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
                ["context slot", card],
            ],
            violations,
        );
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
    await fixture.update({ state: "connected" });
    await playing(page, "idle.webm");
    await observe(
        "Connected",
        [
            ["stage", stage],
            ["clock", page.locator(".pack-clock")],
            ["date", page.locator(".pack-date")],
        ],
        violations,
    );
    assert.deepEqual(fixture.outside, []);
    assert.deepEqual(violations, []);
}
