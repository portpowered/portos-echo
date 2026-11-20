import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { open, playing, refresh, token } from "./pack-session.mjs";

const id = "companion@0.1.0";
const template = "template@0.1.0";
const screenshots = "test-results/screenshots";
const viewport = { width: 960, height: 600 };
const approval = {
    state: "approval",
    id: "companion-approval",
    prompt: "Allow the local action?",
    accept_label: "Allow",
    deny_label: "Deny",
};
const presentations = [
    [{ state: "ready", message: "Ready" }, "idle"],
    [{ state: "connecting" }, "connecting"],
    [{ state: "connected" }, "idle"],
    [{ state: "listening" }, "listening"],
    [{ state: "thinking" }, "thinking"],
    [{ state: "speaking" }, "speaking"],
    [{ state: "muted" }, "muted"],
    [
        {
            state: "link_code",
            user_code: "TEST-CODE",
            verification_uri: "https://127.0.0.1/activate",
            expires_at_unix: 1795174200,
        },
        "setup",
    ],
    [approval, "approval"],
    [{ state: "error", message: "Local action failed", relink: true }, "error"],
];

const settings = (page) =>
    page.getByRole("button", { name: "Settings", exact: true });
const back = (page) => page.getByRole("button", { name: "Back", exact: true });
const selection = (page) => page.getByLabel("Screen pack", { exact: true });
const save = (page) =>
    page.getByRole("button", { name: "Use screen pack", exact: true });

async function download(f, sha256) {
    await f.page.getByLabel("Pack HTTPS URL", { exact: true }).fill(f.peer.url);
    await f.page.getByLabel("Pack SHA-256", { exact: true }).fill(sha256);
    await f.page
        .getByRole("button", { name: "Download pack", exact: true })
        .click();
}

async function saved(page) {
    await page
        .getByText(
            "Screen pack saved. Appearance updates without restarting the bridge.",
            { exact: true },
        )
        .waitFor();
}

async function installedBytes(home) {
    const pack = join(home, "screens", id);
    const hash = createHash("sha256");
    for (const file of (await readdir(pack)).sort()) {
        hash.update(file);
        hash.update(await readFile(join(pack, file)));
    }
    return hash.digest("hex");
}

async function geometry(page, state) {
    const { character, slots, buttons } = await page.evaluate(() => {
        const video = document.querySelector(".pack-stage video:not([hidden])");
        const box = video.getBoundingClientRect();
        const scale = Math.min(
            box.width / video.videoWidth,
            box.height / video.videoHeight,
        );
        const width = video.videoWidth * scale;
        const height = video.videoHeight * scale;
        const visible = (selector) =>
            [...document.querySelectorAll(selector)]
                .filter((element) => element.getClientRects().length)
                .map((element) => ({
                    label: element.className || element.textContent,
                    ...element.getBoundingClientRect().toJSON(),
                }));
        return {
            character: {
                label: "character",
                x: box.x + (box.width - width) / 2,
                y: box.y + (box.height - height) / 2,
                width,
                height,
            },
            slots: visible(".screen-pack [data-pack-slot]"),
            buttons: visible(".screen-pack button, .settings-gear"),
        };
    });
    const overlap = (a, b) =>
        a.x < b.x + b.width &&
        b.x < a.x + a.width &&
        a.y < b.y + b.height &&
        b.y < a.y + a.height;
    for (const box of [character, ...slots, ...buttons]) {
        assert.ok(
            box.x >= 0 &&
                box.y >= 0 &&
                box.x + box.width <= viewport.width &&
                box.y + box.height <= viewport.height,
            `${state}: ${JSON.stringify(box)}`,
        );
    }
    for (const [i, a] of slots.entries()) {
        for (const b of [character, ...slots.slice(i + 1), ...buttons]) {
            // The approval and error actions are native children of their card.
            if (
                ["approval", "error"].includes(state) &&
                a.label.includes("details") &&
                buttons.includes(b) &&
                b.label !== "settings-gear"
            )
                continue;
            assert.ok(
                !overlap(a, b),
                `${state}: ${a.label} overlaps ${b.label}`,
            );
        }
    }
    for (const button of buttons)
        assert.ok(
            !overlap(button, character),
            `${state}: ${button.label} overlaps character`,
        );
    if (["ready", "connected"].includes(state)) {
        for (const label of ["pack-clock", "pack-date"]) {
            const box = slots.find((slot) => slot.label === label);
            assert.ok(
                box.y >= 480 && Math.abs(box.x + box.width / 2 - 480) <= 42,
                label,
            );
        }
    }
}

async function playPresentations(f) {
    const paths = [];
    for (const [state, role] of presentations) {
        await f.update(state);
        const file = await playing(f.page, `${role}.webm`, id);
        paths.push(`${state.state}=${file}`);
        await geometry(f.page, state.state);
        if (["ready", "approval", "link_code"].includes(state.state)) {
            await f.page.waitForFunction(
                () =>
                    document.querySelector("video:not([hidden])")
                        ?.currentTime >= 0.5,
            );
            await f.page.screenshot({
                path: `${screenshots}/companion-${state.state}-960x600.png`,
            });
        }
        if (state.state === "approval") {
            const long = {
                ...approval,
                id: "companion-long-approval",
                prompt: "Review this synthetic request before approving.\n".repeat(
                    18,
                ),
            };
            await f.update(long);
            const card = f.page.locator(".pack-approval");
            assert.equal(
                await card.evaluate(
                    (element) => element.scrollHeight > element.clientHeight,
                ),
                true,
            );
            await f.page.locator("body").click({ position: { x: 1, y: 1 } });
            await f.page.keyboard.press("Tab");
            assert.equal(await f.page.locator(":focus").textContent(), "Allow");
            assert.ok(await card.evaluate((element) => element.scrollTop > 0));
            await f.page.keyboard.press("Enter");
            assert.deepEqual(await f.next(), {
                action: { action: "approve", id: long.id },
            });
        }
    }
    await f.update(presentations[0][0]);
    await playing(f.page, "idle.webm", id);
    await f.update({
        state: "emotion",
        name: "happy",
        frames: ["synthetic"],
        frame_ms: 50,
    });
    await playing(f.page, "happy.webm", id);
    await playing(f.page, "idle.webm", id);
    await f.update(presentations[0][0]);
    console.log(
        `B2 viewport=960x600 overlap=0 ${paths.join(" ")} happy=once->idle long_approval=scroll+keyboard`,
    );
}

async function alternatePlayback(f) {
    const mp4 = await f.context.newPage();
    await mp4.addInitScript(() => {
        HTMLMediaElement.prototype.canPlayType = () => "";
    });
    await mp4.goto(f.page.url());
    await playing(mp4, "idle.mp4", id);
    const { pixel, pageColor } = await mp4
        .locator("video:not([hidden])")
        .evaluate((video) => {
            const canvas = document.createElement("canvas");
            canvas.width = video.videoWidth;
            canvas.height = video.videoHeight;
            const context = canvas.getContext("2d");
            context.drawImage(video, 0, 0);
            return {
                pixel: [...context.getImageData(0, 0, 1, 1).data].slice(0, 3),
                pageColor: getComputedStyle(document.documentElement)
                    .backgroundColor,
            };
        });
    assert.equal(pageColor, "rgb(16, 24, 39)");
    assert.ok(
        pixel.every((value, i) => Math.abs(value - [16, 24, 39][i]) <= 4),
        pixel,
    );
    await mp4.close();
    await f.page.emulateMedia({ reducedMotion: "reduce" });
    await f.update({ state: "listening" });
    assert.equal(
        await f.page.getByRole("heading", { name: "listening" }).isVisible(),
        true,
    );
    await f.page
        .locator(".pack-stage video:not([hidden])")
        .waitFor({ state: "detached" });
    const still = f.page.locator(".pack-stage img");
    assert.equal(await still.isVisible(), true);
    assert.deepEqual(
        await still.evaluate(async (img) => {
            await img.decode();
            return [
                img.naturalWidth,
                img.naturalHeight,
                new URL(img.src).pathname.split("/").at(-1),
            ];
        }),
        [256, 320, "rest.png"],
    );
    console.log(
        `B3 h264=decoded background=${pixel.join(",")} reduced_motion=rest.png state=listening`,
    );
}

test("T3 Companion installs, selects, decodes, preserves selection on failure, and rolls back", async (t) => {
    const archivePath = process.env.SCREEN_COMPANION_ARCHIVE;
    assert.ok(archivePath, "suite owner must supply a Companion archive");
    const f = await open(t, {
        archivePath,
        template,
        viewport,
        initialState: presentations[0][0],
    });
    const { page, home } = f;
    const navigations = [];
    page.on("framenavigated", (frame) => {
        if (frame === page.mainFrame()) navigations.push(frame.url());
    });
    await mkdir(screenshots, { recursive: true });
    await playing(page, "idle.webm", template);
    await settings(page).click();
    await refresh(page);
    await download(f, f.sha256);
    await page.waitForFunction(() => window.packRevision === 1);
    await refresh(page);
    assert.equal((await f.api("/screen-packs/v1")).active, template);
    const preview = page.getByRole("img", { name: "companion rest pose" });
    await preview.scrollIntoViewIfNeeded();
    assert.equal(
        await preview.evaluate(async (img) => {
            await img.decode();
            return img.naturalWidth;
        }),
        256,
    );
    await selection(page).selectOption(id);
    await save(page).click();
    await saved(page);
    await back(page).click();
    await playing(page, "idle.webm", id);
    assert.equal(
        await readFile(join(home, "settings.toml"), "utf8"),
        `screen_pack = "${id}"\n`,
    );
    console.log(
        `B1 digest=${f.sha256} automatic=${template} installed=${id} selected=${id} preview=decoded`,
    );
    await playPresentations(f);
    await alternatePlayback(f);

    const installed = await installedBytes(home);
    await settings(page).click();
    await refresh(page);
    await download(f, "0".repeat(64));
    await page.waitForFunction(async (token) => {
        const response = await fetch(`/screen-packs/v1?token=${token}`);
        return (
            response.ok &&
            (await response.json()).jobs.some(
                (job) =>
                    job.status === "failed" &&
                    job.reason === "checksum_mismatch",
            )
        );
    }, token);
    await refresh(page);
    // The panel exposes the refusal as a job, independently of the active pack.
    await page.getByText(/failed \(checksum mismatch\)/).waitFor();
    const catalog = await f.api("/screen-packs/v1");
    assert.equal(catalog.active, id);
    assert.equal(
        catalog.jobs.filter((job) => job.reason === "checksum_mismatch").length,
        1,
    );
    assert.equal(await installedBytes(home), installed);
    assert.equal(
        await readFile(join(home, "settings.toml"), "utf8"),
        `screen_pack = "${id}"\n`,
    );
    console.log(
        "B4 checksum_mismatch selected=companion@0.1.0 bytes=preserved",
    );

    await selection(page).focus();
    await page.keyboard.press("Shift+Tab");
    await page.keyboard.press("Tab");
    assert.equal(
        await selection(page).evaluate((element) =>
            element.matches(":focus-visible"),
        ),
        true,
    );
    await page.keyboard.press("Home");
    assert.equal(await selection(page).inputValue(), "");
    await page.keyboard.press("Tab");
    assert.equal(
        await save(page).evaluate(
            (element) => element === document.activeElement,
        ),
        true,
    );
    await page.keyboard.press("Enter");
    await saved(page);
    await back(page).click();
    await page.locator(".screen-pack").waitFor({ state: "detached" });
    assert.equal(
        await readFile(join(home, "settings.toml"), "utf8"),
        'screen_pack = ""\n',
    );
    assert.equal((await f.api("/settings/v1")).restart_pending, false);
    assert.deepEqual(navigations, []);
    assert.deepEqual(f.outside, []);
    assert.deepEqual(
        f.peer.requests,
        Array(2).fill("/companion-0.1.0.screenpack.tar.gz"),
    );
    console.log(
        "B5 keyboard=visible selection=empty navigation=0 restart_requests=0",
    );
});
