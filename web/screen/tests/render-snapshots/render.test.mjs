// Render snapshots people can look at: every screen state at 960x600 for the
// default face, a full-page avatar fixture and Settings. PNGs land in
// test-results/screenshots/render/; each set is compared, as one contact
// sheet, with its committed reference in references/.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import {
    openScreen,
    emit,
    hold,
    faceBoxes,
    surfaces,
    intersects,
    shoot,
    width,
} from "./harness.mjs";
import { sheet, matchReference } from "./sheet.mjs";
import { glideRuns } from "./glide.mjs";
import { settingsBody } from "./settings.mjs";

const generated = process.env.SCREEN_PACK_DEFAULT_FIXTURE;
assert.ok(generated, "suite owner must prepare default@0.3.0");
const id = "default@0.3.0";
const dir = join(generated, id);
const manifest = JSON.parse(await readFile(join(dir, "pack.json"), "utf8"));
const card = "The bridge could not complete the last request.";
// [render name, frame, clip, held clip frame]
const STATES = [
    ["ready", { state: "ready", message: "Echo Live ready" }, "idle", 0],
    ["connecting", { state: "connecting" }, "connecting", 0],
    ["connected", { state: "connected" }, "wake", 0],
    ["listening", { state: "listening" }, "listening", 0],
    ["thinking", { state: "thinking" }, "thinking", 22],
    ["speaking", { state: "speaking" }, "speaking", 31],
    ["muted", { state: "muted" }, "muted", 0],
    [
        "link-code",
        {
            state: "link_code",
            user_code: "DEMO-CODE",
            verification_uri: "https://example.invalid/verify",
            expires_at_unix: Date.parse("2026-11-21T09:50:00Z") / 1000,
        },
        "setup",
        0,
    ],
    [
        "approval",
        {
            state: "approval",
            id: "approval-1",
            prompt: "Allow the synthetic fixture to finish its request?",
            accept_label: "Allow",
            deny_label: "Deny",
        },
        "approval",
        0,
    ],
    ["error", { state: "error", message: card, relink: true }, "error", 0],
    [
        "happy",
        { state: "emotion", name: "happy", frame_ms: 80, frames: ["a", "b"] },
        "happy",
        16,
    ],
];
const CARDS = ["link-code", "approval", "error"];

async function checkState(page, name, label) {
    const text = await page.locator("main").innerText();
    assert.doesNotMatch(
        text,
        /speaking|echo live ready|\bready\b|\bstart\b/i,
        `${label} ${name}: no state text`,
    );
    const native = await surfaces(page);
    assert.ok(native.gear && native.clock && native.date, `${name} chrome`);
    assert.equal(!!native.card, CARDS.includes(name), `${name} card`);
    assert.equal(!!native.heading, CARDS.includes(name), `${name} heading`);
    // Time and date share one centre line along the bottom.
    const centre = (box) => box.x + box.width / 2;
    assert.ok(Math.abs(centre(native.clock) - width / 2) <= 1, `${name} clock`);
    assert.ok(Math.abs(centre(native.date) - width / 2) <= 1, `${name} date`);
    const faces = await faceBoxes(page);
    const boxes = Object.entries(native);
    for (const [surface, box] of boxes) {
        assert.ok(
            box.x >= 0 && box.y >= 0 && box.x + box.width <= width,
            `${label} ${name}: ${surface} inside the page`,
        );
        if (name !== "muted")
            assert.ok(
                !intersects(faces.face, box),
                `${label} ${name}: face clears ${surface}`,
            );
        for (const [other, prior] of boxes)
            if (other < surface)
                assert.ok(
                    !intersects(box, prior),
                    `${label} ${name}: ${surface} clears ${other}`,
                );
    }
    return faces;
}

async function renderStates(t, pack, label) {
    const page = await openScreen(t, { pack });
    const renders = [];
    let rest;
    for (const [name, state, clip, frame] of STATES) {
        await emit(page, state);
        await hold(page, `${clip}.webm`, frame);
        if (state.state === "link_code") {
            const details = page.locator(".details");
            assert.deepEqual(await details.locator("p").allTextContents(), [
                state.user_code,
                state.verification_uri,
                "Expires in 9 min",
            ]);
            assert.doesNotMatch(await details.innerText(), /\d{10}/);
            assert.equal(await details.locator("a").count(), 0);
        }
        const faces = await checkState(page, name, label);
        if (name === "ready") rest = faces;
        renders.push(await shoot(page, `${label}-${name}`));
    }
    await matchReference(label, await sheet(renders));
    return rest;
}

test("render: the default face dominates every state, with time, gear and cards clear of it", async (t) => {
    const rest = await renderStates(t, { id, dir, manifest }, "default");
    // Round eyes, each at least 220 CSS px across, the pair spanning 60-70%.
    for (const eye of [rest.left, rest.right]) {
        assert.ok(eye.width >= 220, `eye ${eye.width} px wide`);
        const aspect = eye.width / eye.height;
        assert.ok(Math.abs(aspect - 1) <= 0.05, `eye aspect ${aspect}`);
    }
    const span = (rest.right.x + rest.right.width - rest.left.x) / width;
    assert.ok(span >= 0.6 && span <= 0.7, `eye pair spans ${span}`);
    console.log(`RENDER default eyes ${JSON.stringify(rest)}`);
});

test("render: a full-page avatar keeps the floating gear and the clock usable", async (t) => {
    const fixture = structuredClone(manifest);
    fixture.name = "fullpage";
    // A visible full-bleed backdrop proves the stage covers the whole page.
    fixture.layout.stage = {
        anchor: { x: 50, y: 50 },
        scale: 2,
        background: "#1d3557",
    };
    const pack = { id: "fullpage@1.0.0", dir, manifest: fixture };
    await renderStates(t, pack, "fullpage");
});

test("render: idle to speaking glides from any point of the idle loop, five times", async (t) => {
    const page = await openScreen(t, { pack: { id, dir, manifest } });
    const runs = await glideRuns(page, emit, hold);
    for (const run of runs) {
        assert.ok(run.jump <= 24, `centroid jumped ${JSON.stringify(run)}`);
        assert.equal(run.ended, "speaking.webm", JSON.stringify(run));
    }
    console.log(`RENDER glide ${JSON.stringify(runs)}`);
});

test("render: Settings with linked, unlinked and link-required accounts", async (t) => {
    const renders = [];
    for (const status of ["linked", "unconfigured", "link_required"]) {
        const page = await openScreen(t, {
            pack: { id, dir, manifest },
            settings: settingsBody(status),
        });
        await page.getByRole("button", { name: "Settings", exact: true }).tap();
        await page.getByRole("region", { name: "Accounts" }).waitFor();
        await page.getByRole("region", { name: "Port OS" }).waitFor();
        await page.waitForFunction(
            () => !document.querySelector(".settings-fields[aria-busy=true]"),
        );
        assert.doesNotMatch(
            await page.locator("main").innerText(),
            /settings loaded/i,
        );
        renders.push(await shoot(page, `settings-${status}`));
    }
    await matchReference("settings", await sheet(renders));
});
