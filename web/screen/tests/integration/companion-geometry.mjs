import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { activeVideo, playing } from "./pack-session.mjs";

const id = "companion@0.2.2";
export const viewport = { width: 1280, height: 748 };
const ready = { state: "ready", message: "Ready" };
const thinking = { state: "thinking" };
const link = {
    frame: {
        state: "link_code",
        user_code: "TEST-CODE",
        verification_uri: "https://127.0.0.1/activate",
        expires_at_unix: 1795187160,
    },
    clip: "setup",
    text: ["TEST-CODE", "https://127.0.0.1/activate", "Expires in 2 min"],
};
const presentations = [
    { frame: ready, clip: "idle" },
    link,
    { frame: { state: "speaking" }, clip: "speaking" },
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

async function settledArt(page) {
    await page.waitForFunction((selector) => {
        const video = document.querySelector(selector);
        return (
            video?.currentTime >= 0.65 &&
            !document.querySelector(".pack-stage video[data-leaving]")
        );
    }, activeVideo);
    return page.locator(".screen-pack .pack-stage").evaluate(captureStage, {
        active: true,
    });
}

// With rows, the boxes cover only the character's pixels in those CSS rows.
export async function visibleCharacterBoxes(page, rows) {
    return page
        .locator(".screen-pack .pack-stage")
        .evaluate(captureStage, { rows });
}

function captureStage(stage, { active, clips, handoffs, rows: band } = {}) {
    const host = stage.closest(".screen-pack");
    const videos = [...stage.querySelectorAll("video")];
    const canvas = document.createElement("canvas");
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    const visible = (video) => !video.hidden && video.readyState >= 2;
    const front = (video) =>
        visible(video) && !video.hasAttribute("data-leaving");
    const basename = (video) =>
        new URL(video.dataset.clip).pathname.split("/").at(-1);

    function bounds(video, clip) {
        // Project decoded VP9 art through object-fit: contain into CSS pixels.
        // Intersect with the stage and its host before taking the bounding box.
        const stageBox = stage.getBoundingClientRect();
        const hostBox = host.getBoundingClientRect();
        const leftEdge = Math.max(stageBox.left, hostBox.left, 0);
        const topEdge = Math.max(stageBox.top, hostBox.top, band?.top ?? 0);
        const rightEdge = Math.min(stageBox.right, hostBox.right, innerWidth);
        const bottomEdge = Math.min(
            stageBox.bottom,
            hostBox.bottom,
            band?.bottom ?? innerHeight,
        );
        const box = video.getBoundingClientRect();
        const { videoWidth: width, videoHeight: height } = video;
        const scale = Math.min(box.width / width, box.height / height);
        const x = box.x + (box.width - width * scale) / 2;
        const y = box.y + (box.height - height * scale) / 2;
        const minX = Math.max(0, Math.floor((leftEdge - x) / scale));
        const minY = Math.max(0, Math.floor((topEdge - y) / scale));
        const maxX = Math.min(width, Math.ceil((rightEdge - x) / scale));
        const maxY = Math.min(height, Math.ceil((bottomEdge - y) / scale));
        if (canvas.width !== width) canvas.width = width;
        if (canvas.height !== height) canvas.height = height;
        ctx.clearRect(0, 0, width, height);
        ctx.drawImage(video, 0, 0);
        const { data } = ctx.getImageData(0, 0, width, height);
        const opacity = Number(getComputedStyle(video).opacity);
        let left = width,
            top = height,
            right = 0,
            bottom = 0;
        for (let row = minY; row < maxY; row++) {
            for (let column = minX; column < maxX; column++) {
                const pixel = (row * width + column) * 4;
                // H.264 paints the same stage color into the transparent area.
                const alpha = clip.endsWith(".mp4")
                    ? Math.abs(data[pixel] - 16) >= 24
                        ? 255
                        : 0
                    : data[pixel + 3];
                if (alpha * opacity < 16) continue;
                left = Math.min(left, column);
                top = Math.min(top, row);
                right = Math.max(right, column + 1);
                bottom = Math.max(bottom, row + 1);
            }
        }
        if (right <= left || bottom <= top) return null;
        return {
            left: Math.max(leftEdge, x + left * scale),
            top: Math.max(topEdge, y + top * scale),
            right: Math.min(rightEdge, x + right * scale),
            bottom: Math.min(bottomEdge, y + bottom * scale),
        };
    }

    function sample(video, timing, replay) {
        const clip = basename(video);
        const art = bounds(video, clip);
        const slots = Object.fromEntries(
            Object.entries({
                clock: ".pack-clock",
                date: ".pack-date",
                status: ".pack-status",
                card: ".details[data-pack-slot]",
            }).map(([key, selector]) => {
                const node = host.querySelector(selector);
                const box = node?.getBoundingClientRect();
                return [
                    key,
                    box?.width && box.height
                        ? {
                              text: node.textContent.trim(),
                              ...box.toJSON(),
                          }
                        : null,
                ];
            }),
        );
        const intersections = Object.fromEntries(
            Object.entries(slots).map(([key, box]) => [
                key,
                art && box
                    ? Math.max(
                          0,
                          Math.min(art.right, box.right) -
                              Math.max(art.left, box.left),
                      ) *
                      Math.max(
                          0,
                          Math.min(art.bottom, box.bottom) -
                              Math.max(art.top, box.top),
                      )
                    : 0,
            ]),
        );
        return {
            clip,
            element: videos.indexOf(video),
            replay,
            mediaTime: timing.mediaTime,
            observedAt: performance.now(),
            duration: video.duration,
            art,
            heightPercent: art
                ? (100 * (art.bottom - art.top)) / innerHeight
                : 0,
            center: art ? (art.left + art.right) / 2 : null,
            seq: host.closest("[data-seq]")?.dataset.seq,
            slots,
            intersections,
        };
    }

    if (!clips) {
        if (!active)
            return videos
                .filter(visible)
                .map((video) => bounds(video, basename(video)))
                .filter(Boolean);
        const video = videos.find(front);
        return sample(video, { mediaTime: video.currentTime }, 0);
    }

    const frames = new Map();
    const callbacks = new Map();
    const listeners = new AbortController();
    const seen = [];
    const rows = [];
    const samples = [];
    let previous, latest, lastPrinted;
    let replay = -1;
    const { promise: done, resolve, reject } = Promise.withResolvers();
    // Keep a rejected observer handled even if a test fails before it reads done.
    void done.catch(() => {});
    const mutations = new MutationObserver(() => videos.forEach(record));
    const deadline = setTimeout(() => {
        stop();
        reject(
            new Error(
                `Timed out observing ${clips.join(", ")}: ${JSON.stringify(rows)}`,
            ),
        );
    }, 12000);

    function stop() {
        clearTimeout(deadline);
        mutations.disconnect();
        listeners.abort();
        for (const [video, callback] of callbacks)
            video.cancelVideoFrameCallback(callback);
    }

    function record(video) {
        const timing = frames.get(video);
        if (!timing || !front(video)) return;
        const clip = basename(video);
        if (seen.at(-1) !== clip) seen.push(clip);
        if (!clips.includes(clip)) return;
        const head = previous !== video;
        if (!head && timing.mediaTime <= latest.mediaTime) return;
        if (head) {
            // Keep the last decoded visible sample from the outgoing element.
            if (latest)
                rows.push({
                    ...latest,
                    boundary: "tail",
                    ended: previous.ended,
                });
            previous = video;
            replay++;
        }
        latest = sample(video, timing, replay);
        samples.push(latest);
        if (head || latest.mediaTime - lastPrinted >= 0.2) {
            rows.push({ ...latest, boundary: head ? "head" : "playing" });
            lastPrinted = latest.mediaTime;
        }
        if (replay === handoffs) {
            stop();
            resolve({ rows, samples, seen });
        }
    }

    function watch(video) {
        callbacks.set(
            video,
            video.requestVideoFrameCallback((_, timing) => {
                frames.set(video, timing);
                watch(video);
                record(video);
            }),
        );
    }

    mutations.observe(stage, {
        subtree: true,
        attributes: true,
        attributeFilter: ["hidden", "data-leaving"],
    });
    for (const video of videos) {
        video.addEventListener("loadstart", () => frames.delete(video), {
            signal: listeners.signal,
        });
        watch(video);
    }
    return { done };
}

export function armPlayback(page, clips, handoffs) {
    return page
        .locator(".screen-pack .pack-stage")
        .evaluateHandle(captureStage, { clips, handoffs });
}

export async function playbackTrace(observer) {
    try {
        return await observer.evaluate(({ done }) => done);
    } finally {
        await observer.dispose();
    }
}

function diagnostic(row) {
    return JSON.stringify(row, (key, value) =>
        key === "slots" ? undefined : value,
    );
}

// Beside a card the character stands 110 clip pixels left of centre; the clip
// is 720 pixels tall and fills the display's height.
const cardCenter = viewport.width / 2 - (110 * viewport.height) / 720;

function checkGeometry(sample, expectedCenter, card) {
    const { width, height } = viewport;
    const { art, center, heightPercent, slots, intersections } = sample;
    assert.ok(
        art && heightPercent >= 94 && heightPercent <= 100,
        diagnostic(sample),
    );
    assert.ok(
        Math.abs(center - expectedCenter) <= 32,
        `center=${center}, expected ${expectedCenter} ±32 CSS px: ${diagnostic(sample)}`,
    );
    assert.ok(art.right > art.left && art.left >= 0 && art.top >= 0);
    assert.ok(art.right <= width && art.bottom <= height);
    assert.equal(slots.clock?.text, "03:04 PM");
    assert.equal(slots.date?.text, "11/20/2026");
    assert.equal(Boolean(slots.status), card);
    assert.equal(Boolean(slots.card), card);
    for (const [name, slot] of Object.entries(slots)) {
        // Time and date are drawn over the character, bottom right.
        if (!["clock", "date"].includes(name))
            assert.equal(
                intersections[name],
                0,
                `${name}: ${diagnostic(sample)}`,
            );
        if (!slot) continue;
        assert.ok(slot.text && slot.width > 0 && slot.height > 0, name);
        assert.ok(slot.left >= 0 && slot.top >= 0, name);
        assert.ok(slot.right <= width && slot.bottom <= height, name);
    }
}

async function checkNative(page, { text, buttons }) {
    for (const label of text ?? [])
        await page.getByText(label, { exact: true }).waitFor();
    for (const name of buttons ?? [])
        await page
            .getByRole("button", { name, exact: true })
            .click({ trial: true });
}

// A held clip (one named in held) shows no frame while its pose holds, and
// its last frame may end the clip unseen. A replay of one is known to have
// played through by the time from its first frame to the next replay's. The
// first replay's first frame may be seen late, behind the entry and reveal,
// so that time is checked from the second replay on.
export function checkHandoffs(rows, expectedClips, { held = [] } = {}) {
    const heads = rows.filter(({ boundary }) => boundary === "head");
    const tails = rows.filter(({ boundary }) => boundary === "tail");
    assert.deepEqual(
        heads.map(({ clip }) => clip),
        expectedClips,
    );
    assert.equal(tails.length, heads.length - 1);
    for (const [index, tail] of tails.entries()) {
        const head = heads[index + 1];
        assert.equal(tail.replay, index);
        assert.notEqual(tail.element, head.element);
        const holds = held.includes(tail.clip);
        assert.ok(
            tail.ended && (holds || tail.duration - tail.mediaTime <= 0.12),
            diagnostic(tail),
        );
        assert.ok(
            head.mediaTime <= 0.1 && head.observedAt > tail.observedAt,
            diagnostic(head),
        );
        if (holds && index > 0)
            assert.ok(
                head.observedAt - heads[index].observedAt >=
                    (2000 * tail.duration) / 3,
                diagnostic(head),
            );
        else if (!holds)
            assert.ok(
                rows.some(
                    (row) =>
                        row.replay === index &&
                        row.boundary === "playing" &&
                        row.mediaTime > tail.duration / 3 &&
                        row.mediaTime < (2 * tail.duration) / 3,
                ),
            );
    }
}

export async function checkHeldLinkCode(f) {
    const { page } = f;
    const observer = await armPlayback(page, ["setup.webm"], 3);
    await f.update(link.frame);
    const presentation = page.getByRole("region", {
        name: "Screen presentation",
    });
    const seq = await presentation.getAttribute("data-seq");
    const url = page.url();
    const { rows, samples } = await playbackTrace(observer);
    for (const row of rows) console.log(`LINK_REPLAY ${diagnostic(row)}`);
    checkHandoffs(rows, Array(4).fill("setup.webm"), {
        held: ["setup.webm"],
    });
    assert.ok(seq);
    assert.equal(await presentation.getAttribute("data-seq"), seq);
    assert.equal(page.url(), url);
    for (const sample of samples) {
        assert.equal(sample.seq, seq);
        for (const text of link.text)
            assert.ok(sample.slots.card?.text.includes(text), text);
    }
    await checkNative(page, link);
    assert.deepEqual(f.outside, []);
    // Collect both complete replay traces before reporting a center excursion.
    const excursions = rows.filter(
        ({ center }) => Math.abs(center - cardCenter) > 32,
    );
    assert.equal(
        excursions.length,
        0,
        `held Link Code center excursions beyond ${cardCenter.toFixed(2)} ±32 CSS px: ${excursions.map(({ replay, center }) => `${replay}/${center?.toFixed(2)}`).join(", ")}`,
    );
    for (const sample of samples) checkGeometry(sample, cardCenter, true);
}

async function checkEntry(f, frame) {
    const { page } = f;
    const entry = await armPlayback(
        page,
        ["setup-enter.webm", "setup.webm"],
        1,
    );
    await f.update(frame);
    const { rows, samples } = await playbackTrace(entry);
    for (const row of rows) console.log(`LINK_ENTRY ${diagnostic(row)}`);
    checkHandoffs(rows, ["setup-enter.webm", "setup.webm"]);
    checkGeometry(rows.at(-2), cardCenter, true);
    checkGeometry(rows.at(-1), cardCenter, true);
    assert.ok(
        Math.abs(rows[0].center - viewport.width / 2) <= 32,
        diagnostic(rows[0]),
    );
    assert.ok(samples.some(({ center }) => center > 480 && center < 590));
}

async function checkReturnHome(f, screenshots) {
    const { page } = f;
    const returning = await armPlayback(page, ["thinking.webm"], 1);
    await f.update(thinking);
    const { rows } = await playbackTrace(returning);
    console.log(`LINK_DEPARTURE ${diagnostic(rows[0])}`);
    assert.ok(rows[0].center < viewport.width / 2 - 32, diagnostic(rows[0]));
    const settled = await settledArt(page);
    console.log(`B6 state=thinking ${diagnostic(settled)}`);
    checkGeometry(settled, viewport.width / 2, false);
    await page.screenshot({
        path: `${screenshots}/companion-thinking-1280x748.png`,
    });
}

// Clips are fetched ahead and replay from memory, so an entry clip can only
// be pending while its first fetch is: hold that from before the pack is used.
export async function checkInterruptedEntry(f, select) {
    const { page } = f;
    const entry = (url) => url.pathname.endsWith("/setup-enter.webm");
    const pending = Promise.withResolvers();
    const release = Promise.withResolvers();
    await page.route(entry, async (route) => {
        if (route.request().resourceType() === "media") pending.resolve();
        await release.promise;
        await route.continue().catch(() => {});
    });
    try {
        await select();
        await f.update(link.frame);
        await pending.promise;
        await f.update(thinking);
        await playing(page, "thinking.webm", id);
        const observer = await armPlayback(page, ["thinking.webm"], 1);
        release.resolve();
        const { seen } = await playbackTrace(observer);
        assert.deepEqual(seen, ["thinking.webm"]);
        assert.equal(
            await page.getByText("TEST-CODE", { exact: true }).isVisible(),
            false,
        );
        console.log(
            "LINK_INTERRUPTED pending=setup-enter.webm replacement=thinking.webm replay=thinking.webm",
        );
    } finally {
        release.resolve();
    }
    assert.deepEqual(f.outside, []);
}

export async function checkCompanionDisplay(f) {
    const screenshots = "test-results/screenshots";
    await mkdir(screenshots, { recursive: true });
    for (const presentation of presentations) {
        const { frame, clip } = presentation;
        if (presentation === link) await checkEntry(f, frame);
        else await f.update(frame);
        await playing(f.page, `${clip}.webm`, id);
        const art = await settledArt(f.page);
        console.log(`B6 state=${frame.state} ${diagnostic(art)}`);
        checkGeometry(
            art,
            presentation.text ? cardCenter : viewport.width / 2,
            Boolean(presentation.text),
        );
        await checkNative(f.page, presentation);
        if (!["approval", "error"].includes(frame.state)) {
            await f.page.screenshot({
                path: `${screenshots}/companion-${frame.state}-1280x748.png`,
            });
        }
        if (presentation === link) await checkReturnHome(f, screenshots);
    }
    assert.deepEqual(f.outside, []);
}

// Every frame of the pack is a separately generated image of one character.
// Her skin and jacket must keep one colour through the blink, the mouth
// shapes and the hand-over from the still, or she flickers. The widest
// channel range was 10.3 levels before the frames were matched to the neutral
// one and is 2.4 after; the budget is 5.
export async function checkSteadyColour({ page }) {
    const rows = await page
        .locator(".screen-pack .pack-stage")
        .evaluate(async (stage) => {
            const clip = stage.querySelector("video[data-clip]").dataset.clip;
            const canvas = document.createElement("canvas");
            canvas.width = 768;
            canvas.height = 720;
            const ctx = canvas.getContext("2d", { willReadFrequently: true });
            // Flat areas of the cheek and the jacket, in clip pixels.
            const patches = {
                skin: [412, 204, 18, 18],
                jacket: [336, 476, 28, 28],
            };
            const read = (source) => {
                ctx.clearRect(0, 0, canvas.width, canvas.height);
                ctx.drawImage(source, 0, 0);
                return Object.entries(patches).flatMap(([, [x, y, w, h]]) => {
                    const { data } = ctx.getImageData(x, y, w, h);
                    const sum = [0, 0, 0];
                    for (let i = 0; i < data.length; i += 4)
                        for (let c = 0; c < 3; c++) sum[c] += data[i + c];
                    return sum.map((value) => value / (w * h));
                });
            };
            const still = stage.querySelector("img:not(.pack-mouth)");
            await still.decode();
            const rows = [["rest.png", ...read(still)]];
            // The lip-sync stills are whole frames of her too.
            for (const mouth of stage.querySelectorAll(".pack-mouth")) {
                await mouth.decode();
                const name = new URL(mouth.src).pathname.split("/").at(-1);
                rows.push([name, ...read(mouth)]);
            }
            const video = document.createElement("video");
            video.muted = true;
            video.preload = "auto";
            const event = (name) =>
                new Promise((resolve, reject) => {
                    video.addEventListener(name, resolve, { once: true });
                    video.addEventListener("error", reject, { once: true });
                });
            // Idle frames 14 and 15 are the first blink. She breathes, so
            // each is read beside frame 26 or 25, the same height on the way
            // back up. The speaking frames span every mouth shape.
            const frames = {
                idle: [0, 14, 26, 15, 25, 40],
                speaking: Array.from({ length: 41 }, (_, i) => i),
            };
            const fps = { idle: 10, speaking: 30 };
            for (const [role, list] of Object.entries(frames)) {
                video.src = clip.replace(/[^/?]+\.webm/, `${role}.webm`);
                await event("loadeddata");
                for (const frame of list) {
                    video.currentTime = (frame + 0.5) / fps[role];
                    await event("seeked");
                    rows.push([`${role}:${frame}`, ...read(video)]);
                }
            }
            video.removeAttribute("src");
            video.load();
            return rows;
        });
    const blink = ["idle:14", "idle:26", "idle:15", "idle:25"];
    const groups = [
        rows.filter(([name]) => !blink.includes(name)),
        rows.filter(([name]) => blink.slice(0, 2).includes(name)),
        rows.filter(([name]) => blink.slice(2).includes(name)),
    ];
    const ranges = [0, 1, 2, 3, 4, 5].map((channel) =>
        Math.max(
            ...groups.map((group) => {
                const values = group.map((row) => row[channel + 1]);
                return Math.max(...values) - Math.min(...values);
            }),
        ),
    );
    console.log(
        `B7 colour frames=${rows.length} skin_range=${ranges
            .slice(0, 3)
            .map((value) => value.toFixed(2))} jacket_range=${ranges
            .slice(3)
            .map((value) => value.toFixed(2))}`,
    );
    for (const file of ["mouth-half.png", "mouth-open.png", "mouth-a.png"])
        assert.ok(
            rows.some(([name]) => name === file),
            `${file} is measured`,
        );
    assert.ok(
        Math.max(...ranges) <= 5,
        `colour moves between frames: ${JSON.stringify(
            rows.map((row) => [
                row[0],
                ...row.slice(1).map((value) => Math.round(value)),
            ]),
        )}`,
    );
}

// While she speaks only her mouth may change: every mouth frame is the neutral
// one outside the mouth region, so her hair, outlines and jacket hold still.
// Outside the region's box (clip pixels, feather and resampling included),
// the lossless stills must equal rest.png exactly; the lossy speaking clip's
// frames may differ from its first by codec noise only. Mean RGBA levels.
const MOUTH_BOX = [340, 225, 443, 292];
const STILL_NOISE = 0;
const CLIP_NOISE = 0.1;
export async function checkStillBody({ page }) {
    const rows = await page
        .locator(".screen-pack .pack-stage")
        .evaluate(async (stage, [left, top, right, bottom]) => {
            const clip = stage.querySelector("video[data-clip]").dataset.clip;
            const canvas = document.createElement("canvas");
            canvas.width = 768;
            canvas.height = 720;
            const ctx = canvas.getContext("2d", { willReadFrequently: true });
            const read = (source) => {
                ctx.clearRect(0, 0, canvas.width, canvas.height);
                ctx.drawImage(source, 0, 0);
                return ctx.getImageData(0, 0, 768, 720).data;
            };
            const outside = (a, b) => {
                let total = 0;
                let count = 0;
                for (let y = 0; y < 720; y++) {
                    for (let x = 0; x < 768; x++) {
                        if (x >= left && x < right && y >= top && y < bottom)
                            continue;
                        const i = (y * 768 + x) * 4;
                        for (let c = 0; c < 4; c++)
                            total += Math.abs(a[i + c] - b[i + c]);
                        count += 4;
                    }
                }
                return total / count;
            };
            const still = stage.querySelector("img:not(.pack-mouth)");
            await still.decode();
            const rest = read(still);
            const rows = [];
            for (const mouth of stage.querySelectorAll(".pack-mouth")) {
                await mouth.decode();
                const name = new URL(mouth.src).pathname.split("/").at(-1);
                if (name !== "rest.png")
                    rows.push(["still", name, outside(read(mouth), rest)]);
            }
            const video = document.createElement("video");
            video.muted = true;
            video.preload = "auto";
            const event = (name) =>
                new Promise((resolve, reject) => {
                    video.addEventListener(name, resolve, { once: true });
                    video.addEventListener("error", reject, { once: true });
                });
            video.src = clip.replace(/[^/?]+\.webm/, "speaking.webm");
            await event("loadeddata");
            let first = null;
            // The first 41 frames hold every mouth shape of the clip.
            for (let frame = 0; frame <= 40; frame++) {
                video.currentTime = (frame + 0.5) / 30;
                await event("seeked");
                const pixels = read(video);
                if (first === null) first = pixels;
                else
                    rows.push([
                        "clip",
                        `speaking:${frame}`,
                        outside(pixels, first),
                    ]);
            }
            video.removeAttribute("src");
            video.load();
            return rows;
        }, MOUTH_BOX);
    const worst = (kind) =>
        Math.max(...rows.filter(([k]) => k === kind).map(([, , v]) => v));
    console.log(
        `B8 mouth-only stills=${rows.filter(([k]) => k === "still").length} ` +
            `still_outside_mouth=${worst("still").toFixed(3)} ` +
            `clip_outside_mouth=${worst("clip").toFixed(3)} ` +
            `budgets=${STILL_NOISE},${CLIP_NOISE}`,
    );
    for (const [kind, name, value] of rows)
        assert.ok(
            value <= (kind === "still" ? STILL_NOISE : CLIP_NOISE),
            `${name} differs outside the mouth by ${value}`,
        );
    assert.ok(rows.some(([, name]) => name === "mouth-open.png"));
}

// The idle home screen is what the display shows most of the day. On the Echo
// a playing clip costs about 16% of a core with nothing changing and about 3%
// more for each frame it shows a second, all off the page's main thread. The
// idle loop is therefore one slow breath at 10 fps and then a hold with
// blinks, and the page does no work of its own between frames. Measured here
// in headless Chromium over the first seven seconds of the idle loop:
//
//                             30 fps loop   10 fps loop   breath, hold   budget
//     frames shown per second        30.0          10.0             4.6        7
//     main thread busy, ms/s          3.1           3.2             3.9       30
//     script, ms/s                   0.62          0.69            0.73       10
//     layout and style, ms/s         0.21          0.23            0.18        5
//
// On the Echo the three builds take 88%, 45% and 24% of a core (WebView
// renderer plus the screen app). The frame rate is what separates them; the
// main-thread budgets keep per-frame page work from creeping in beside it.
const idleBudget = { frames: 7, task: 30, script: 10, layout: 5 };

async function count(window_ms) {
    const proto = HTMLVideoElement.prototype;
    const original = {
        frame: proto.requestVideoFrameCallback,
        timeout: window.setTimeout,
        interval: window.setInterval,
        animation: window.requestAnimationFrame,
    };
    let armed = 0;
    const counted = (call) =>
        function (...args) {
            armed++;
            return call.apply(this, args);
        };
    proto.requestVideoFrameCallback = counted(original.frame);
    window.setTimeout = counted(original.timeout);
    window.setInterval = counted(original.interval);
    window.requestAnimationFrame = counted(original.animation);
    let frames = 0;
    let done = false;
    for (const video of document.querySelectorAll(".pack-stage video")) {
        const tick = () => {
            if (done) return;
            frames++;
            original.frame.call(video, tick);
        };
        original.frame.call(video, tick);
    }
    const start = performance.now();
    await new Promise((resolve) =>
        original.timeout.call(window, resolve, window_ms),
    );
    done = true;
    proto.requestVideoFrameCallback = original.frame;
    window.setTimeout = original.timeout;
    window.setInterval = original.interval;
    window.requestAnimationFrame = original.animation;
    const seconds = (performance.now() - start) / 1000;
    return { frames: frames / seconds, armed: armed / seconds };
}

// The cost of the clip on the stage, measured over window_ms once it has
// settled: the frames the display is given a second, with every timer and
// frame callback the page arms itself, and the main thread's work over all
// but the first second and the last two.
async function playbackCost(page, label, window_ms) {
    // A loop that has just ended stays in view, paused, until its replay
    // shows its first frame.
    await page.waitForFunction((selector) => {
        const video = document.querySelector(selector);
        return (
            video?.currentTime >= 0.4 &&
            !video.paused &&
            !document.querySelector(".pack-stage video[data-leaving]")
        );
    }, activeVideo);
    // The causes, each on its own: nothing recolours or resamples the video
    // while it plays, and the loop plays at its own rate.
    const styles = await page.locator(activeVideo).evaluate((video) => {
        const chain = [];
        for (let node = video; node; node = node.parentElement) {
            const style = getComputedStyle(node);
            chain.push([
                node.className || node.tagName,
                style.filter,
                style.backdropFilter,
                style.mixBlendMode,
                style.animationName,
            ]);
        }
        return { chain, paused: video.paused, rate: video.playbackRate };
    });
    for (const [node, ...effects] of styles.chain)
        assert.deepEqual(
            effects,
            ["none", "none", "normal", "none"],
            `${node} adds a per-frame effect to the ${label} clip`,
        );
    assert.deepEqual([styles.paused, styles.rate], [false, 1]);

    const session = await page.context().newCDPSession(page);
    await session.send("Performance.enable");
    const metrics = async () =>
        Object.fromEntries(
            (await session.send("Performance.getMetrics")).metrics.map(
                ({ name, value }) => [name, value],
            ),
        );
    const counting = page.evaluate(count, window_ms);
    await page.waitForTimeout(1000);
    const before = await metrics();
    await page.waitForTimeout(window_ms - 3000);
    const after = await metrics();
    const seconds = after.Timestamp - before.Timestamp;
    const rate = (name) => ((after[name] - before[name]) * 1000) / seconds;
    const cost = {
        task: rate("TaskDuration"),
        script: rate("ScriptDuration"),
        layout: rate("LayoutDuration") + rate("RecalcStyleDuration"),
    };
    const shown = await counting;
    await session.detach();
    console.log(
        `B8 ${label} frames_per_s=${shown.frames.toFixed(1)} armed_per_s=${shown.armed.toFixed(2)} task_ms_per_s=${cost.task.toFixed(1)} script_ms_per_s=${cost.script.toFixed(2)} layout_ms_per_s=${cost.layout.toFixed(2)} layouts=${after.LayoutCount - before.LayoutCount} recalcs=${after.RecalcStyleCount - before.RecalcStyleCount}`,
    );
    // A loop that re-arms itself every frame would arm 10 or more a second.
    assert.ok(shown.armed <= 2, `${label} arms ${shown.armed} timers a second`);
    return { shown, cost };
}

function checkBudget(label, { shown, cost }, budget) {
    assert.ok(
        shown.frames <= budget.frames,
        `${label} shows ${shown.frames.toFixed(1)} frames a second`,
    );
    for (const [name, value] of Object.entries(cost))
        assert.ok(
            value <= budget[name],
            `${label} ${name} cost is ${value.toFixed(2)} ms a second`,
        );
}

export async function checkIdleCost({ page }) {
    const measured = await playbackCost(page, "idle", 7000);
    assert.ok(measured.shown.frames > 0, "idle shows no frame");
    checkBudget("idle", measured, idleBudget);
}

// Error, approval, the link code and muted show a pose that does not move,
// yet as 30 fps clips the display decoded and redrew 30 frames a second of
// it: Error measured about 71% of a core on the Echo. Each is now a hold
// that codes only its cross-fades and its last frame, so while the pose
// holds the display is given no frame at all. Measured here in headless
// Chromium over four seconds of each state:
//
//                                30 fps clip   hold   budget
//     frames shown per second,
//       error                         30.0      0.0        2
//       approval                      29.8      0.0        2
//       link code                     29.3      1.2        2
//       muted                         27.8      0.0        2
//     main-thread task ms per second,
//       error                         14.6      3.1
//       approval                      14.9      2.8
//       link code                     19.4      7.1
//       muted                         27.4      2.9
//     ms from the state to her pose,
//       error, approval, muted     30-83    47-62     1500
//       link code (with its entry)   622      621     1500
//
// The link code's loop is two seconds, so a replay shows two frames in a
// window; the others loop every ten seconds. The main-thread budgets are the
// idle loop's.
const stillBudget = { ...idleBudget, frames: 2 };
// From the state change to her new pose on the stage, link code entry
// included; a clip revealed on its last frame takes 2 to 10 seconds.
const revealBudget = 1500;
const stills = {
    error: {
        frame: { state: "error", message: "Local action failed", relink: true },
        clip: "error",
    },
    approval: {
        frame: presentations.find(({ clip }) => clip === "approval").frame,
        clip: "approval",
    },
    link_code: { frame: link.frame, clip: "setup" },
    muted: { frame: { state: "muted" }, clip: "muted" },
};

export const checkStillCost =
    (...names) =>
    async ({ page, update }) => {
        for (const name of names) {
            const { frame, clip } = stills[name];
            const asked = performance.now();
            await update(frame);
            await playing(page, `${clip}.webm`, id);
            // The page reveals a clip on the first frame it presents once it
            // plays; a hold must not put that frame seconds away.
            const reveal = performance.now() - asked;
            console.log(`B8 ${name} revealed_ms=${reveal.toFixed(0)}`);
            assert.ok(
                reveal <= revealBudget,
                `${name} shows ${reveal.toFixed(0)} ms after the state changes`,
            );
            const measured = await playbackCost(page, name, 4000);
            // The pose is still on the stage, and still playing, at the end.
            await page.waitForFunction(
                ({ selector, name }) => {
                    const video = document.querySelector(selector);
                    return (
                        video &&
                        !video.paused &&
                        new URL(video.dataset.clip).pathname.endsWith(name)
                    );
                },
                { selector: activeVideo, name: `/${id}/${clip}.webm` },
                { timeout: 3000 },
            );
            checkBudget(name, measured, stillBudget);
        }
    };
