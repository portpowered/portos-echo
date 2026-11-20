import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { activeVideo, open, playing, refresh } from "./pack-session.mjs";

const id = "companion@0.2.0";
const viewport = { width: 1280, height: 748 };
const ready = { state: "ready", message: "Ready" };
const presentations = [
    { frame: ready, clip: "idle" },
    { frame: { state: "thinking" }, clip: "thinking" },
    { frame: { state: "speaking" }, clip: "speaking" },
    {
        frame: {
            state: "link_code",
            user_code: "TEST-CODE",
            verification_uri: "https://127.0.0.1/activate",
            expires_at_unix: 1795187160,
        },
        clip: "setup",
        text: ["TEST-CODE", "https://127.0.0.1/activate", "Expires in 2 min"],
    },
    {
        frame: {
            state: "approval",
            id: "companion-display-approval",
            prompt: "Allow the local action?",
            accept_label: "Allow",
            deny_label: "Deny",
        },
        clip: "approval",
        text: ["Allow the local action?"],
        buttons: ["Allow", "Deny"],
    },
    {
        frame: { state: "error", message: "Local action failed", relink: true },
        clip: "error",
        text: ["Local action failed"],
        buttons: ["Relink", "Dismiss"],
    },
];

async function selectCompanion(f) {
    const button = (name) => f.page.getByRole("button", { name, exact: true });
    await button("Settings").click();
    await refresh(f.page);
    await f.page.getByLabel("Pack HTTPS URL", { exact: true }).fill(f.peer.url);
    await f.page.getByLabel("Pack SHA-256", { exact: true }).fill(f.sha256);
    await button("Download pack").click();
    await f.page.waitForFunction(() => window.packRevision === 1);
    await refresh(f.page);
    await f.page.getByLabel("Screen pack", { exact: true }).selectOption(id);
    await button("Use screen pack").click();
    await f.page
        .getByText(
            "Screen pack saved. Appearance updates without restarting the bridge.",
            { exact: true },
        )
        .waitFor();
    await button("Back").click();
}

async function settledArt(page) {
    await page.waitForFunction((selector) => {
        const video = document.querySelector(selector);
        return (
            video?.currentTime >= 0.65 &&
            !document.querySelector(".pack-stage video[data-leaving]")
        );
    }, activeVideo);
    const [art] = await visibleCharacterBoxes(page);
    return art;
}

export async function visibleCharacterBoxes(page) {
    return page.locator(".screen-pack .pack-stage").evaluate((stage) => {
        // Project decoded VP9 art through object-fit: contain into CSS pixels.
        // Intersect with the stage and its host before taking the bounding box.
        const stageBox = stage.getBoundingClientRect();
        const host = stage.closest(".screen-pack").getBoundingClientRect();
        const leftEdge = Math.max(stageBox.left, host.left, 0);
        const topEdge = Math.max(stageBox.top, host.top, 0);
        const rightEdge = Math.min(stageBox.right, host.right, innerWidth);
        const bottomEdge = Math.min(stageBox.bottom, host.bottom, innerHeight);
        const boxes = [];
        for (const video of stage.querySelectorAll("video:not([hidden])")) {
            const box = video.getBoundingClientRect();
            const { videoWidth: width, videoHeight: height } = video;
            const scale = Math.min(box.width / width, box.height / height);
            const x = box.x + (box.width - width * scale) / 2;
            const y = box.y + (box.height - height * scale) / 2;
            const minX = Math.max(0, Math.floor((leftEdge - x) / scale));
            const minY = Math.max(0, Math.floor((topEdge - y) / scale));
            const maxX = Math.min(width, Math.ceil((rightEdge - x) / scale));
            const maxY = Math.min(height, Math.ceil((bottomEdge - y) / scale));
            const canvas = document.createElement("canvas");
            canvas.width = width;
            canvas.height = height;
            const ctx = canvas.getContext("2d");
            ctx.drawImage(video, 0, 0);
            const { data } = ctx.getImageData(0, 0, width, height);
            const opacity = Number(getComputedStyle(video).opacity);
            let left = width,
                top = height,
                right = 0,
                bottom = 0;
            for (let row = minY; row < maxY; row++) {
                for (let column = minX; column < maxX; column++) {
                    const alpha = data[(row * width + column) * 4 + 3];
                    if (alpha * opacity < 16) continue;
                    left = Math.min(left, column);
                    top = Math.min(top, row);
                    right = Math.max(right, column + 1);
                    bottom = Math.max(bottom, row + 1);
                }
            }
            if (right <= left || bottom <= top) continue;
            boxes.push({
                left: Math.max(leftEdge, x + left * scale),
                top: Math.max(topEdge, y + top * scale),
                right: Math.min(rightEdge, x + right * scale),
                bottom: Math.min(bottomEdge, y + bottom * scale),
            });
        }
        return boxes;
    });
}

const overlaps = (a, b) =>
    a.left < b.right &&
    b.left < a.right &&
    a.top < b.bottom &&
    b.top < a.bottom;

async function checkGeometry(page, { frame, text, buttons }, art) {
    const { width, height } = viewport;
    const artHeight = art.bottom - art.top;
    const center = (art.right + art.left) / 2;
    const expectedCenter = width / (text ? 3 : 2);
    assert.ok(
        artHeight >= 0.75 * height && artHeight <= 0.85 * height,
        `${frame.state} height=${artHeight}`,
    );
    assert.ok(
        Math.abs(center - expectedCenter) <= 0.05 * (width / 2),
        `${frame.state} center=${center}, expected ${expectedCenter}`,
    );
    assert.ok(art.right > art.left && art.left >= 0 && art.top >= 0);
    assert.ok(art.right <= width && art.bottom <= height);
    const slots = await page
        .locator(".screen-pack [data-pack-slot]")
        .evaluateAll((nodes) =>
            nodes.map((node) => ({
                slot: node.className,
                text: node.textContent.trim(),
                ...node.getBoundingClientRect().toJSON(),
            })),
        );
    assert.equal(
        slots.find((slot) => slot.slot === "pack-clock")?.text,
        "03:04 PM",
    );
    assert.equal(
        slots.find((slot) => slot.slot === "pack-date")?.text,
        "11/20/2026",
    );
    assert.equal(slots.length, text ? 4 : 2);
    for (const slot of slots) {
        assert.ok(slot.text && slot.width > 0 && slot.height > 0, slot.slot);
        assert.ok(slot.left >= 0 && slot.top >= 0, slot.slot);
        assert.ok(slot.right <= width && slot.bottom <= height, slot.slot);
        assert.ok(
            !overlaps(art, slot),
            `${frame.state}: art overlaps ${slot.slot}`,
        );
    }
    for (const label of text ?? [])
        assert.equal(
            await page.getByText(label, { exact: true }).isVisible(),
            true,
        );
    for (const name of buttons ?? [])
        await page
            .getByRole("button", { name, exact: true })
            .click({ trial: true });
    console.log(
        `B6 viewport=1280x748 state=${frame.state} art=${JSON.stringify(art)} height=${artHeight.toFixed(1)} center=${center.toFixed(1)} overlap=0`,
    );
}

export async function checkCompanionDisplay(t) {
    const archivePath = process.env.SCREEN_COMPANION_ARCHIVE;
    assert.ok(archivePath, "suite owner must supply a Companion archive");
    const f = await open(t, {
        archivePath,
        template: "template@0.1.0",
        viewport,
        initialState: ready,
    });
    await selectCompanion(f);
    const screenshots = "test-results/screenshots";
    await mkdir(screenshots, { recursive: true });
    for (const presentation of presentations) {
        const { frame, clip } = presentation;
        await f.update(frame);
        await playing(f.page, `${clip}.webm`, id);
        const art = await settledArt(f.page);
        await checkGeometry(f.page, presentation, art);
        if (!["approval", "error"].includes(frame.state)) {
            await f.page.screenshot({
                path: `${screenshots}/companion-${frame.state}-1280x748.png`,
            });
        }
    }
    assert.deepEqual(f.outside, []);
}
