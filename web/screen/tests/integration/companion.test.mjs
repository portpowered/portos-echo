import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import {
    armPlayback,
    checkCompanionDisplay,
    checkHandoffs,
    checkHeldLinkCode,
    playbackTrace,
    visibleCharacterBoxes,
    viewport as displayViewport,
} from "./companion-geometry.mjs";
import { activeVideo, open, playing, refresh, token } from "./pack-session.mjs";

const id = "companion@0.2.1";
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

function openCompanion(t, viewport) {
    const archivePath = process.env.SCREEN_COMPANION_ARCHIVE;
    const initialState = presentations[0][0];
    assert.ok(archivePath, "suite owner must supply a Companion archive");
    return open(t, { archivePath, template, viewport, initialState });
}

async function installCompanion(f) {
    const { page } = f;
    await playing(page, "idle.webm", template);
    await settings(page).click();
    await refresh(page);
    await download(f, f.sha256);
    await page.waitForFunction(() => window.packRevision === 1);
    await refresh(page);
}

async function useCompanion(page) {
    await selection(page).selectOption(id);
    await save(page).click();
    await saved(page);
    await back(page).click();
    await playing(page, "idle.webm", id);
}

const withSelected = (check) => async (t) => {
    const f = await openCompanion(t, displayViewport);
    await installCompanion(f);
    await useCompanion(f.page);
    await check(f);
};

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
    const art = await visibleCharacterBoxes(page);
    assert.ok(art.length, `${state}: decoded character is visible`);
    const characters = art.map((box) => ({ label: "character", ...box }));
    const { slots, buttons } = await page.evaluate(() => {
        const visible = (selector) =>
            [...document.querySelectorAll(selector)]
                .filter((element) => element.getClientRects().length)
                .map((element) => ({
                    label: element.className || element.textContent,
                    ...element.getBoundingClientRect().toJSON(),
                }));
        return {
            slots: visible(".screen-pack [data-pack-slot]"),
            buttons: visible(".screen-pack button, .settings-gear"),
        };
    });
    const overlap = (a, b) =>
        a.left < b.right &&
        b.left < a.right &&
        a.top < b.bottom &&
        b.top < a.bottom;
    const boxes = [...characters, ...slots, ...buttons];
    const { width, height } = viewport;
    for (const { left, top, right, bottom, label } of boxes)
        assert.ok(
            left >= 0 && top >= 0 && right <= width && bottom <= height,
            `${state}: ${label} bounds=${[left, top, right, bottom]}`,
        );
    // Ready's transparent hit area intentionally spans the pack content.
    const paintedButtons = buttons.filter(
        (button) => state !== "ready" || button.label !== "home-tap",
    );
    for (const [i, a] of slots.entries()) {
        for (const b of [
            ...characters,
            ...slots.slice(i + 1),
            ...paintedButtons,
        ]) {
            // The approval and error actions are native children of their card.
            if (
                ["approval", "error"].includes(state) &&
                a.label.includes("details") &&
                paintedButtons.includes(b) &&
                b.label !== "settings-gear"
            )
                continue;
            assert.ok(
                !overlap(a, b),
                `${state}: ${a.label} overlaps ${JSON.stringify(b)}`,
            );
        }
    }
    for (const button of paintedButtons)
        assert.ok(
            characters.every((character) => !overlap(button, character)),
            `${state}: ${button.label} overlaps character`,
        );
    if (["ready", "connected"].includes(state)) {
        for (const label of ["pack-clock", "pack-date"]) {
            const box = slots.find((slot) => slot.label === label);
            assert.ok(
                box.top >= 480 &&
                    Math.abs((box.left + box.right) / 2 - 480) <= 42,
                label,
            );
        }
    }
    console.log(`B2 art state=${state} boxes=${JSON.stringify(art)} overlap=0`);
}

async function checkReadyTap(f) {
    const tap = f.page.getByRole("button", {
        name: "Begin session",
        exact: true,
    });
    assert.deepEqual(
        await tap.evaluate((element) => {
            const style = getComputedStyle(element);
            return {
                background: style.backgroundColor,
                image: style.backgroundImage,
                border: style.borderWidth,
                shadow: style.boxShadow,
                text: element.textContent,
            };
        }),
        {
            background: "rgba(0, 0, 0, 0)",
            image: "none",
            border: "0px",
            shadow: "none",
            text: "",
        },
    );
    await tap.focus();
    await f.page.keyboard.press("Tab");
    await f.page.keyboard.press("Shift+Tab");
    assert.ok(
        await tap.evaluate((element) => {
            const { outlineStyle, outlineWidth } = getComputedStyle(element);
            return (
                element.matches(":focus-visible") &&
                outlineStyle === "solid" &&
                parseFloat(outlineWidth) > 0
            );
        }),
        "Ready tap area has a visible keyboard focus outline",
    );
    await f.page.keyboard.press("Enter");
    assert.deepEqual(await f.next(), {
        action: { action: "tap", target: "primary" },
    });
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
                (selector) =>
                    document.querySelector(selector)?.currentTime >= 0.5,
                activeVideo,
            );
            await f.page.screenshot({
                path: `${screenshots}/companion-${state.state}-960x600.png`,
            });
        }
        if (state.state === "ready") await checkReadyTap(f);
        if (state.state === "approval") {
            const long = {
                ...approval,
                id: "companion-long-approval",
                prompt: "Review this synthetic request before approving.\n".repeat(
                    18,
                ),
            };
            await f.update(long);
            const prompt = f.page.locator(".pack-approval > p");
            const allow = f.page.getByRole("button", {
                name: "Allow",
                exact: true,
            });
            assert.equal(
                await prompt.evaluate(
                    (element) => element.scrollHeight > element.clientHeight,
                ),
                true,
            );
            await prompt.hover();
            await f.page.mouse.wheel(0, 260);
            await f.page.waitForFunction(
                () =>
                    document.querySelector(".pack-approval > p").scrollTop > 0,
            );
            await allow.click({ trial: true });
            await prompt.focus();
            await f.page.keyboard.press("Tab");
            assert.equal(await f.page.locator(":focus").textContent(), "Allow");
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
        `B2 viewport=960x600 overlap=0 ${paths.join(" ")} ready_tap=transparent+focus+primary happy=once->idle long_approval=scroll+keyboard`,
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
        .locator(activeVideo)
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
    const setup = ["setup-enter.mp4", "setup.mp4", "setup.mp4"];
    const observer = await armPlayback(mp4, setup, 2);
    await f.update(
        presentations.find(([state]) => state.state === "link_code")[0],
    );
    const { rows } = await playbackTrace(observer);
    checkHandoffs(rows, setup);
    assert.ok(rows.every(({ art }) => art));
    await mp4.close();
    await f.page.emulateMedia({ reducedMotion: "reduce" });
    await f.update({ state: "listening" });
    const presentation = f.page.getByRole("region", {
        name: "Screen presentation",
    });
    assert.equal(await presentation.getAttribute("data-ui-state"), "listening");
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
        [768, 720, "rest.png"],
    );
    console.log(
        `B3 h264=decoded background=${pixel.join(",")} setup=entry->loop->replay reduced_motion=rest.png state=listening`,
    );
}

test("T3 Companion installs, selects, decodes, preserves selection on failure, and rolls back", async (t) => {
    const f = await openCompanion(t, viewport);
    const { page, home } = f;
    const navigations = [];
    page.on("framenavigated", (frame) => {
        if (frame === page.mainFrame()) navigations.push(frame.url());
    });
    await mkdir(screenshots, { recursive: true });
    await installCompanion(f);
    assert.equal((await f.api("/screen-packs/v1")).active, template);
    const preview = page.getByRole("img", { name: "companion rest pose" });
    await preview.scrollIntoViewIfNeeded();
    assert.equal(
        await preview.evaluate(async (img) => {
            await img.decode();
            return img.naturalWidth;
        }),
        768,
    );
    await useCompanion(page);
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
    const [queued] = await Promise.all([
        page.waitForResponse(
            (response) =>
                new URL(response.url()).pathname ===
                    "/screen-packs/v1/install" &&
                response.request().method() === "POST",
        ),
        download(f, "0".repeat(64)),
    ]);
    assert.equal(queued.status(), 202);
    const { job: jobId } = await queued.json();
    const result = await page.evaluate(
        async ({ token, jobId }) => {
            const signal = AbortSignal.timeout(5000);
            for (;;) {
                const response = await fetch(
                    `/screen-packs/v1?token=${token}`,
                    {
                        cache: "no-store",
                        signal,
                    },
                );
                if (!response.ok)
                    throw new Error(`Pack catalog returned ${response.status}`);
                const { jobs } = await response.json();
                const job = jobs.find(({ id }) => id === jobId);
                if (job?.status === "failed" || job?.status === "installed")
                    return job;
                await new Promise((resolve) => requestAnimationFrame(resolve));
            }
        },
        { token, jobId },
    );
    assert.equal(result.status, "failed");
    assert.equal(result.reason, "checksum_mismatch");
    await refresh(page);
    // The panel exposes the refusal as a job, independently of the active pack.
    await page
        .getByText(`Download ${jobId}: failed (checksum mismatch)`, {
            exact: true,
        })
        .waitFor();
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
        "B4 checksum_mismatch selected=companion@0.2.1 bytes=preserved",
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
    await page.keyboard.press("End");
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
        Array(2).fill("/companion-0.2.1.screenpack.tar.gz"),
    );
    console.log(
        "B5 keyboard=visible selection=empty navigation=0 restart_requests=0",
    );
});

test(
    "T3 Companion visible art fills the display while leaving room for native content",
    withSelected(checkCompanionDisplay),
);

test(
    "T3 Companion Link Code stays centered through natural replays",
    withSelected(checkHeldLinkCode),
);
