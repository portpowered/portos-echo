import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { visibleCharacterBoxes } from "../../../web/screen/tests/integration/companion-geometry.mjs";
import {
    activeVideo,
    open,
    playing,
    refresh,
} from "../../../web/screen/tests/integration/pack-session.mjs";

const id = "companion@0.2.2";
const viewport = { width: 1280, height: 748 };
const ready = { state: "ready", message: "Ready" };
const sampleFrame = 24;
const fps = 30;
const alphaCutoff = 16;
const minimumPoseDelta = 32;
const maximumClosedError = 8;
const cards = [
    {
        frame: {
            state: "link_code",
            user_code: "TEST-CODE",
            verification_uri: "https://127.0.0.1/activate",
            expires_at_unix: 1795187160,
        },
        clip: "setup",
        texts: ["TEST-CODE", "https://127.0.0.1/activate", "Expires in 2 min"],
        buttons: [],
    },
    {
        frame: {
            state: "approval",
            id: "companion-presentation-approval",
            prompt: "Allow the local action?",
            accept_label: "Allow",
            deny_label: "Deny",
        },
        clip: "approval",
        texts: ["Allow the local action?"],
        buttons: ["Allow", "Deny"],
    },
    {
        frame: { state: "error", message: "Local action failed", relink: true },
        clip: "error",
        texts: ["Local action failed"],
        buttons: ["Relink", "Dismiss"],
    },
];

async function installCompanion({ page, peer, sha256, api }) {
    const button = (name) => page.getByRole("button", { name, exact: true });
    await button("Settings").click();
    await refresh(page);
    await page.getByLabel("Pack HTTPS URL", { exact: true }).fill(peer.url);
    await page.getByLabel("Pack SHA-256", { exact: true }).fill(sha256);
    await button("Download pack").click();
    await page.waitForFunction(() => window.packRevision === 1);
    await refresh(page);
    await page.getByLabel("Screen pack", { exact: true }).selectOption(id);
    await button("Use screen pack").click();
    await page
        .getByText(
            "Screen pack saved. Appearance updates without restarting the bridge.",
            { exact: true },
        )
        .waitFor();
    assert.equal((await api("/screen-packs/v1")).active, id);
    await button("Back").click();
    await page.waitForFunction(() =>
        [
            ...document.querySelectorAll(".screen-pack link[rel=stylesheet]"),
        ].some(
            (link) =>
                new URL(link.href).pathname.endsWith(
                    "/companion@0.2.2/companion-clock.css",
                ) && link.sheet,
        ),
    );
}

async function seek(page, frame) {
    await page.locator(activeVideo).evaluate(
        async (video, { frame, fps }) => {
            video.pause();
            const seeked = new Promise((resolve) =>
                video.addEventListener("seeked", resolve, { once: true }),
            );
            // A held clip presents the frame it holds, which began earlier.
            const presented = new Promise((resolve) => {
                const check = (_, { mediaTime }) => {
                    if (Math.round(mediaTime * fps) <= frame) resolve();
                    else video.requestVideoFrameCallback(check);
                };
                video.requestVideoFrameCallback(check);
            });
            video.currentTime = (frame + 0.25) / fps;
            await Promise.all([seeked, presented]);
        },
        { frame, fps },
    );
}

async function settle(f, frame, clip) {
    await f.update(frame);
    await playing(f.page, `${clip}.webm`, id);
    await f.page.waitForFunction((selector) => {
        const video = document.querySelector(selector);
        return (
            video &&
            getComputedStyle(video).opacity === "1" &&
            !document.querySelector(".pack-stage video[data-leaving]")
        );
    }, activeVideo);
    await seek(f.page, sampleFrame);
}

function capture(page) {
    return page.locator(activeVideo).evaluateHandle((video) => {
        const canvas = document.createElement("canvas");
        canvas.width = video.videoWidth;
        canvas.height = video.videoHeight;
        const ctx = canvas.getContext("2d");
        ctx.drawImage(video, 0, 0);
        return ctx.getImageData(0, 0, canvas.width, canvas.height);
    });
}

async function comparePoses(page, readyFrame, listeningFrame) {
    const { pixels, mean } = await page.evaluate(
        ({ readyFrame, listeningFrame, alphaCutoff }) => {
            const a = readyFrame.data;
            const b = listeningFrame.data;
            const background = [16, 24, 39];
            let pixels = 0;
            let sum = 0;
            for (let i = 0; i < a.length; i += 4) {
                if (Math.max(a[i + 3], b[i + 3]) < alphaCutoff) continue;
                pixels++;
                for (const [channel, bg] of background.entries()) {
                    const compositeA = Math.round(
                        bg + ((a[i + channel] - bg) * a[i + 3]) / 255,
                    );
                    const compositeB = Math.round(
                        bg + ((b[i + channel] - bg) * b[i + 3]) / 255,
                    );
                    sum += Math.abs(compositeA - compositeB);
                }
            }
            return { pixels, mean: sum / (3 * pixels) };
        },
        { readyFrame, listeningFrame, alphaCutoff },
    );
    console.log(
        `POSE time=${((sampleFrame + 0.25) / fps).toFixed(6)}s alpha=${alphaCutoff} pixels=${pixels} min_delta=${minimumPoseDelta.toFixed(1)} delta=${mean.toFixed(2)}`,
    );
    assert.ok(mean > minimumPoseDelta, "Listening pose must differ from Ready");
}

async function compareMouth(
    page,
    readyFrame,
    listeningFrame,
    halfFrame,
    openFrame,
) {
    const [closed, half, open] = await page.evaluate(
        ({ readyFrame, listeningFrame, halfFrame, openFrame }) => {
            // Same 50 × 31 crop as the generator's established mouth oracle.
            // Offsets account for the sprite registration and frame motion only.
            function lip({ width, data }, dx, dy) {
                const rgb = [];
                for (let y = 229 + dy; y < 260 + dy; y++) {
                    for (let x = 476 + dx; x < 526 + dx; x++) {
                        const i = (y * width + x) * 4;
                        rgb.push(data[i], data[i + 1], data[i + 2]);
                    }
                }
                return rgb;
            }
            const listening = lip(listeningFrame, 32, -11);
            return [
                lip(readyFrame, 0, 0),
                lip(halfFrame, 5, 2),
                lip(openFrame, 5, 2),
            ].map(
                (reference) =>
                    reference.reduce(
                        (sum, channel, i) =>
                            sum + Math.abs(channel - listening[i]),
                        0,
                    ) / reference.length,
            );
        },
        { readyFrame, listeningFrame, halfFrame, openFrame },
    );
    console.log(
        `MOUTH crop=50x31 closed_limit=${maximumClosedError.toFixed(1)} closed=${closed.toFixed(2)} half=${half.toFixed(2)} open=${open.toFixed(2)}`,
    );
    assert.ok(
        closed <= maximumClosedError && closed < half && closed < open,
        "Listening mouth must match the closed reference",
    );
}

const overlaps = (a, b) =>
    a.left < b.right &&
    b.left < a.right &&
    a.top < b.bottom &&
    b.top < a.bottom;
const centerX = ({ left, right }) => (left + right) / 2;

async function checkLayout(page, state, card) {
    const [art] = await visibleCharacterBoxes(page);
    assert.ok(art, `${state} must render visible video pixels`);
    const height = art.bottom - art.top;
    const { width } = viewport;
    const artCenter = centerX(art);
    assert.ok(
        height >= 0.75 * viewport.height && height <= 0.85 * viewport.height,
        `${state} art height=${height}`,
    );
    assert.ok(
        Math.abs(artCenter - width / (card ? 3 : 2)) <= 32,
        `${state} art center=${artCenter}`,
    );
    assert.ok(
        art.left >= 0 &&
            art.top >= 0 &&
            art.right <= width &&
            art.bottom <= viewport.height,
    );
    const centers = [];
    const native = await page
        .locator(".screen-pack :is(h1, .details p, .controls button):visible")
        .evaluateAll((nodes) =>
            nodes.map((node) => node.getBoundingClientRect().toJSON()),
        );
    for (const [role, text] of [
        ["clock", "03:04 PM"],
        ["date", "11/20/2026"],
    ]) {
        const slot = page.locator(`.screen-pack .pack-${role}`);
        assert.ok(await slot.isVisible(), `${state} ${role} is visible`);
        const box = await slot.evaluate((node) => ({
            ...node.getBoundingClientRect().toJSON(),
            align: getComputedStyle(node).textAlign,
            text: node.textContent,
        }));
        const center = centerX(box);
        assert.equal(box.text, text);
        assert.equal(box.align, "center");
        assert.ok(
            Math.abs(center - width / 2) <= 6.4,
            `${state} ${role} center=${center}`,
        );
        assert.ok(
            box.top >= art.bottom && box.bottom <= viewport.height,
            `${state} ${role} clears the art`,
        );
        assert.ok(
            native.every((rect) => !overlaps(rect, box)),
            `${state} ${role} overlaps a native control`,
        );
        centers.push(center);
    }
    for (const rect of native)
        assert.ok(
            !overlaps(rect, art),
            `${state} native content overlaps the art`,
        );
    for (const text of card?.texts ?? [])
        assert.ok(
            await page.getByText(text, { exact: true }).isVisible(),
            text,
        );
    for (const name of card?.buttons ?? [])
        await page
            .getByRole("button", { name, exact: true })
            .click({ trial: true });
    console.log(
        `LAYOUT state=${state} art_height=${height.toFixed(1)} art_center=${artCenter.toFixed(1)} clock_center=${centers[0].toFixed(2)} date_center=${centers[1].toFixed(2)} overlap=0`,
    );
}

test("T3 release-staged Companion has a closed-mouth Listening pose and centered clock and date", async (t) => {
    assert.ok(
        process.env.SCREEN_COMPANION_ARCHIVE,
        "provide the staged release archive",
    );
    const f = await open(t, {
        archivePath: process.env.SCREEN_COMPANION_ARCHIVE,
        template: "template@0.1.0",
        viewport,
        initialState: ready,
    });
    await installCompanion(f);
    const { page } = f;
    const stage = page.locator(".screen-pack .pack-stage");
    assert.equal(
        await stage.evaluate((node) => getComputedStyle(node).backgroundColor),
        "rgb(16, 24, 39)",
    );
    console.log(`BROWSER archive=${id} sha256=${f.sha256} viewport=1280x748`);
    const directory = "test-results/screenshots";
    await mkdir(directory, { recursive: true });
    const snapshot = (state) =>
        page.screenshot({
            path: join(directory, `companion-0.2.2-${state}.png`),
        });
    await settle(f, ready, "idle");
    await checkLayout(page, "ready");
    const readyFrame = await capture(page);
    await snapshot("ready");
    await settle(f, { state: "listening" }, "listening");
    await checkLayout(page, "listening");
    const listeningFrame = await capture(page);
    await snapshot("listening");
    await comparePoses(page, readyFrame, listeningFrame);

    await settle(f, { state: "thinking" }, "thinking");
    await checkLayout(page, "thinking");
    await settle(f, { state: "speaking" }, "speaking");
    await checkLayout(page, "speaking");
    // The existing speech timeline holds half at 19–23 and open at 24–31.
    await seek(page, 21);
    const halfFrame = await capture(page);
    await seek(page, 28);
    const openFrame = await capture(page);
    await compareMouth(page, readyFrame, listeningFrame, halfFrame, openFrame);

    for (const card of cards) {
        await settle(f, card.frame, card.clip);
        await checkLayout(page, card.frame.state, card);
    }
    await snapshot("error");
    assert.deepEqual(f.outside, []);
});
