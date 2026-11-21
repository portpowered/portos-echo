import { test } from "node:test";
import assert from "node:assert/strict";
import { pageFor, emit } from "../browser.mjs";
import { frame } from "../fixtures.mjs";

const clip = (name) => ({
    webm: `${name}.webm`,
    mp4: `${name}.mp4`,
    mode: "loop",
});
const character = {
    rest: "rest.png",
    states: Object.fromEntries(
        ["idle", "listening", "thinking", "speaking", "error", "approval"].map(
            (name) => [name, { clip: clip(name) }],
        ),
    ),
    emotes: { wave: clip("wave") },
};
const layout = {
    safe_area: { x: 10, y: 10, width: 80, height: 80 },
    slots: {
        clock: { x: 5, y: 5 },
        date: { x: 5, y: 12 },
        link_code: { x: 20, y: 25 },
        status: { x: 0, y: 0 },
        approval: { x: 95, y: 95 },
        error: { x: 0, y: 10 },
    },
    stage: { anchor: { x: 50, y: 50 }, scale: 1, background: "#101827" },
};
const manifest = {
    format: 1,
    name: "sample",
    version: "1.0.0",
    character,
    layout,
};

async function packPage(t, pack = manifest, vp9 = true) {
    return pageFor(
        t,
        "?events=%2Fevents%3Ftoken%3Dsynthetic-screen-pair&actions=%2Factions%3Ftoken%3Dsynthetic-screen-pair",
        false,
        async (page) => {
            await page.evaluate(
                ({ pack, vp9 }) => {
                    const marker = document.createElement("meta");
                    marker.name = "echo-packs";
                    marker.content = "v1";
                    document.head.append(marker);
                    window.EventSource = class extends EventTarget {
                        constructor(url) {
                            super();
                            this.url = url;
                            window.streams.push(this);
                        }
                        close() {}
                    };
                    window.packData = pack;
                    window.packRevision = 0;
                    window.packActive = "sample@1.0.0";
                    window.packStatus = 200;
                    window.packRequests = [];
                    // Clip bytes are fetched ahead. Unless a test serves
                    // them, each clip keeps loading from its own URL.
                    window.clipRequests = [];
                    window.fetch = async (url, options) => {
                        if (/\.(webm|mp4)\?/.test(url)) {
                            window.clipRequests.push(
                                new URL(url).pathname.split("/").at(-1),
                            );
                            return window.serveClips
                                ? { status: 200, blob: async () => new Blob() }
                                : { status: 404 };
                        }
                        window.packRequests.push({
                            url,
                            cache: options.cache,
                            signal: options.signal,
                        });
                        if (url.includes("pack.json"))
                            return {
                                status: window.packStatus,
                                json: async () => window.packData,
                            };
                        return {
                            status: 200,
                            json: async () => ({
                                version: 1,
                                revision: window.packRevision,
                                active: window.packActive,
                                warning: null,
                                installed: [
                                    {
                                        id: "sample@1.0.0",
                                        name: "sample",
                                        version: "1.0.0",
                                        rest_url:
                                            "/screen-packs/v1/sample@1.0.0/rest.png",
                                    },
                                ],
                                jobs: [],
                            }),
                        };
                    };
                    // Media readiness/first-frame events are controlled here. Actual
                    // generated codec decode belongs to the generated-template witness.
                    Object.defineProperty(HTMLImageElement.prototype, "src", {
                        set(value) {
                            this.dataset.src = value;
                        },
                        get() {
                            return this.dataset.src;
                        },
                    });
                    HTMLImageElement.prototype.decode = () =>
                        window.pauseDecode
                            ? new Promise((resolve) => {
                                  window.finishDecode = resolve;
                              })
                            : Promise.resolve();
                    Object.defineProperty(HTMLMediaElement.prototype, "src", {
                        set(value) {
                            this.dataset.src = value;
                        },
                        get() {
                            return this.dataset.src;
                        },
                    });
                    HTMLMediaElement.prototype.canPlayType = () =>
                        vp9 ? "probably" : "";
                    HTMLMediaElement.prototype.load = function () {};
                    HTMLMediaElement.prototype.pause = function () {};
                    HTMLMediaElement.prototype.play = function () {
                        return window.rejectPlay
                            ? Promise.reject(new Error("controlled"))
                            : Promise.resolve();
                    };
                    HTMLVideoElement.prototype.requestVideoFrameCallback =
                        function (callback) {
                            this.firstFrame = callback;
                            return 1;
                        };
                    HTMLVideoElement.prototype.cancelVideoFrameCallback =
                        function () {
                            this.firstFrame = undefined;
                        };
                },
                { pack, vp9 },
            );
        },
    );
}
const send = (page, seq, state) =>
    emit(page, frame(seq, JSON.stringify(state)));
const loaded = (page) =>
    page.evaluate(() => {
        const video = [...document.querySelectorAll("video")].find(
            (v) => v.hidden && v.dataset.src,
        );
        video.dispatchEvent(new Event("loadeddata"));
        video.firstFrame?.();
    });
const end = (page) =>
    page.evaluate(() =>
        document
            .querySelector("video:not([hidden]):not([data-leaving])")
            .dispatchEvent(new Event("ended")),
    );
const hiddenSource = (page) =>
    page.evaluate(() =>
        new URL(
            [...document.querySelectorAll("video")].find(
                (v) => v.hidden && v.dataset.src,
            ).dataset.src,
        ).pathname
            .split("/")
            .at(-1),
    );
const visible = (page) =>
    page
        .locator("video:not([hidden]):not([data-leaving])")
        .evaluate((node) => new URL(node.src).pathname.split("/").at(-1));
const reload = (page, revision) =>
    page.evaluate((revision) => {
        window.packRevision = revision;
        window.streams[0].dispatchEvent(
            new MessageEvent("screen-pack-reload", {
                data: JSON.stringify({ version: 1, revision }),
            }),
        );
    }, revision);

test("pack page: covered presentation coalesces reloads without racing settings reads", async (t) => {
    const page = await packPage(t);
    await page.locator(".screen-pack").waitFor();
    const count = await page.evaluate(() => {
        document.querySelector("section").hidden = true;
        return window.packRequests.length;
    });
    await reload(page, 1);
    await reload(page, 2);
    assert.equal(await page.evaluate(() => window.packRequests.length), count);
    await page.evaluate(() => {
        document.querySelector("section").hidden = false;
    });
    await page.waitForFunction(
        (count) => window.packRequests.length === count + 2,
        count,
    );
    await page.waitForFunction(() =>
        document
            .querySelector(".pack-stage img")
            ?.dataset.src.includes("revision=2"),
    );
    await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
    await page.evaluate(() => {
        document.querySelector("section").hidden = true;
    });
    await reload(page, 3);
    await page.evaluate(() => {
        document.querySelector("section").hidden = false;
    });
    assert.equal(
        await page.evaluate(() => window.packRequests.length),
        count + 2,
    );
});

test("pack page: clips are fetched ahead once and replay from memory under their own name", async (t) => {
    const page = await packPage(t);
    await page.locator(".screen-pack").waitFor();
    await page.evaluate(() => {
        window.serveClips = true;
    });
    await send(page, 1, { state: "ready", message: "Ready" });
    // The first clip still loads from its URL; every clip is fetched once,
    // the states of a reply first.
    assert.equal(await hiddenSource(page), "idle.webm");
    await loaded(page);
    await page.waitForFunction(() => window.clipRequests.length === 7);
    assert.deepEqual(await page.evaluate(() => window.clipRequests), [
        "speaking.webm",
        "idle.webm",
        "listening.webm",
        "thinking.webm",
        "error.webm",
        "approval.webm",
        "wave.webm",
    ]);
    await send(page, 2, { state: "listening" });
    const next = await page.evaluate(() => {
        const video = [...document.querySelectorAll("video")].find(
            (v) => v.hidden && v.dataset.src,
        );
        return { src: video.dataset.src, clip: video.dataset.clip };
    });
    assert.match(next.src, /^blob:/);
    assert.equal(
        new URL(next.clip).pathname.split("/").at(-1),
        "listening.webm",
    );
    await loaded(page);
    await send(page, 3, { state: "ready", message: "Ready" });
    await loaded(page);
    assert.equal(await page.evaluate(() => window.clipRequests.length), 7);
});

test("pack page: state src sequence, first-frame handoff, urgent cuts and Emotion fallbacks", async (t) => {
    const page = await packPage(t);
    await page.locator(".screen-pack").waitFor();
    await send(page, 1, { state: "ready", message: "Ready" });
    await loaded(page);
    const observed = [await visible(page)];
    await send(page, 2, { state: "listening" });
    // Old video remains visible until the first decoded next frame.
    assert.equal(await visible(page), "idle.webm");
    await page.evaluate(() => {
        const back = document.querySelector("video[hidden]");
        back.dispatchEvent(new Event("loadeddata"));
    });
    assert.equal(await visible(page), "idle.webm");
    await page.evaluate(() =>
        document.querySelector("video[hidden]").firstFrame(),
    );
    observed.push(await visible(page));
    // States cut at once; speaking starts after SPEAK_START_MS and the
    // mouth closes only after SPEAK_HOLD_MS of silence.
    for (const [seq, state, wait] of [
        [3, "thinking", 0],
        [4, "speaking", 100],
        [5, "ready", 500],
    ]) {
        const prior = await visible(page);
        await send(page, seq, { state, message: "" });
        assert.equal(await visible(page), prior);
        const clip = `${state === "ready" ? "idle" : state}.webm`;
        const requested = () =>
            page.evaluate(
                (clip) =>
                    [...document.querySelectorAll("video")].some((video) =>
                        (video.dataset.src ?? "").includes(clip),
                    ),
                clip,
            );
        if (wait) {
            await page.clock.runFor(wait - 1);
            assert.equal(await requested(), false, `${state} waits`);
            await page.clock.runFor(1);
        }
        assert.equal(await requested(), true, `${state} requested`);
        await loaded(page);
        observed.push(await visible(page));
    }
    assert.equal(await page.locator("main h1").count(), 0, "no state text");
    assert.deepEqual(observed, [
        "idle.webm",
        "listening.webm",
        "thinking.webm",
        "speaking.webm",
        "idle.webm",
    ]);
    await send(page, 6, {
        state: "emotion",
        name: "wave",
        frames: ["old"],
        frame_ms: 10,
    });
    await loaded(page);
    assert.equal(await visible(page), "wave.webm");
    assert.equal(await page.getByLabel("Animation frame").count(), 0);
    await end(page);
    await loaded(page);
    assert.equal(await visible(page), "idle.webm");
    await send(page, 7, {
        state: "emotion",
        name: "unknown",
        frames: ["frame-a", "frame-b"],
        frame_ms: 10,
    });
    assert.equal(
        await page.getByLabel("Animation frame").textContent(),
        "frame-a",
    );
    assert.equal(await page.locator(".pack-stage").isVisible(), false);
    await page.clock.runFor(10);
    assert.equal(
        await page.getByLabel("Animation frame").textContent(),
        "frame-b",
    );
    assert.equal(await page.locator("video").count(), 2);
    // Time and date stay on screen through every state.
    assert.equal(await page.locator(".pack-clock").isVisible(), true);
    const requests = await page.evaluate(() =>
        window.packRequests.map((r) => r.url),
    );
    assert.ok(
        requests.every(
            (url) =>
                new URL(url).searchParams.get("token") ===
                "synthetic-screen-pair",
        ),
    );
    console.log(`pack clip sequence: ${observed.join(" -> ")}`);
});

test("pack page: MP4 capability fallback, idle/rest, character-only, and reduced motion", async (t) => {
    const page = await packPage(
        t,
        {
            ...manifest,
            layout: undefined,
            character: {
                ...character,
                states: { idle: { clip: clip("idle") } },
            },
        },
        false,
    );
    await page.locator(".screen-pack").waitFor();
    await send(page, 1, { state: "muted" });
    await loaded(page);
    assert.equal(await visible(page), "idle.mp4");
    await page.evaluate(() => {
        window.motionSettled = new Promise((resolve) => {
            matchMedia("(prefers-reduced-motion: reduce)").addEventListener(
                "change",
                resolve,
                { once: true },
            );
        });
    });
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.evaluate(() => window.motionSettled.then(() => undefined));
    assert.equal(
        await page.locator("video:not([hidden]):not([data-leaving])").count(),
        0,
    );
    await send(page, 2, { state: "speaking" });
    assert.equal(await page.locator(".pack-stage img").isVisible(), true);
    await page.evaluate(() => {
        window.packData = { format: 1, character: { rest: "rest.png" } };
    });
    await reload(page, 1);
    await page.waitForFunction(() => window.packRequests.length === 4);
    await send(page, 3, { state: "ready", message: "Rest only" });
    assert.equal(await page.locator(".pack-stage img").isVisible(), true);
    assert.equal(
        await page.locator("video:not([hidden]):not([data-leaving])").count(),
        0,
    );
});

test("pack page: layout clock/link coordinates, safe-area approval containment and no-pack reset", async (t) => {
    const page = await packPage(t, { ...manifest, character: undefined });
    await page.locator(".screen-pack").waitFor();
    await send(page, 1, { state: "ready", message: "Clock" });
    assert.equal(await page.locator("video").count(), 0);
    assert.equal(await page.locator(".pack-clock").isVisible(), true);
    const clock = await page.locator(".pack-clock").boundingBox();
    assert.equal(Math.round(clock.x), 179);
    assert.equal(Math.round(clock.y), 112);
    assert.equal(await page.locator(".pack-clock").textContent(), "12:00 AM");
    const date = page.locator(".pack-date");
    const dateBox = await date.boundingBox();
    assert.equal(Math.round(dateBox.x), 179);
    assert.equal(Math.round(dateBox.y), 157);
    assert.equal(await date.textContent(), "1/1/1970");
    await send(page, 2, {
        state: "link_code",
        user_code: "ABCD",
        verification_uri: "local",
        expires_at_unix: 100,
    });
    const code = await page.locator(".pack-link-code").boundingBox();
    assert.equal(Math.round(code.x), 333);
    assert.equal(Math.round(code.y), 240);
    await send(page, 3, {
        state: "approval",
        id: "local",
        prompt: "Approve this action",
        accept_label: "Allow",
        deny_label: "Deny",
    });
    for (const viewport of [
        { width: 1280, height: 800 },
        { width: 640, height: 600 },
    ]) {
        await page.setViewportSize(viewport);
        await page
            .getByRole("button", { name: "Allow", exact: true })
            .scrollIntoViewIfNeeded();
        const card = await page.locator(".pack-approval").boundingBox();
        const safe = await page.locator(".screen-pack").boundingBox();
        assert.ok(card.x >= safe.x && card.y >= safe.y);
        assert.ok(card.x + card.width <= safe.x + safe.width + 1);
        assert.ok(card.y + card.height <= safe.y + safe.height + 1);
        assert.equal(
            await page
                .getByRole("button", { name: "Allow", exact: true })
                .isVisible(),
            true,
        );
        await page.screenshot({
            path: `test-results/screenshots/pack-layout-${viewport.width}x${viewport.height}.png`,
        });
    }
    await page.evaluate(() => {
        window.packActive = null;
    });
    await reload(page, 1);
    await page.waitForFunction(() => !document.querySelector(".screen-pack"));
    assert.equal(await page.locator(".pack-clock").count(), 0);
    assert.equal(await page.locator("h1").textContent(), "approval");
    assert.equal(await page.locator("section").getAttribute("data-seq"), "3");
    await page.getByRole("button", { name: "Allow", exact: true }).focus();
    const requestCount = await page.evaluate(() => {
        window.retainedControl = document.activeElement;
        const count = window.packRequests.length;
        window.streams[0].dispatchEvent(new Event("open"));
        return count;
    });
    await page.waitForFunction(
        (count) => window.packRequests.length > count,
        requestCount,
    );
    assert.equal(
        await page.evaluate(
            () =>
                window.retainedControl === document.activeElement &&
                window.retainedControl.isConnected,
        ),
        true,
    );
});

test("pack page: reload freshness, reconnect, safe failures and disposal", async (t) => {
    const page = await packPage(t);
    await page.locator(".screen-pack").waitFor();
    await send(page, 1, { state: "ready", message: "Ready" });
    await loaded(page);
    await reload(page, 1);
    await page.waitForFunction(() => window.packRequests.length === 4);
    assert.equal(await page.locator("[data-ui-state=ready]").count(), 1);
    await page.waitForFunction(() =>
        document
            .querySelector(".pack-stage img")
            ?.dataset.src.includes("revision=1"),
    );
    await loaded(page);
    assert.equal(await page.locator("section").getAttribute("data-seq"), "1");
    await reload(page, 1);
    assert.equal(await page.evaluate(() => window.packRequests.length), 4);
    await page.evaluate(() => {
        window.pauseDecode = true;
        window.packData.character.rest = "replacement.png";
    });
    await reload(page, 2);
    await page.waitForFunction(() => !!window.finishDecode);
    const loading = await page.evaluate(() => {
        const count = window.packRequests.length;
        window.streams[0].dispatchEvent(new Event("open"));
        return count;
    });
    assert.equal(
        await page.evaluate(() => window.packRequests.length),
        loading,
    );
    assert.match(
        await page.locator(".pack-stage img").getAttribute("data-src"),
        /\/rest\.png\?/,
    );
    assert.equal(await visible(page), "idle.webm");
    await page.evaluate(() => {
        window.pauseDecode = false;
        window.finishDecode();
    });
    await page.waitForFunction(() =>
        document
            .querySelector(".pack-stage img")
            ?.dataset.src.includes("replacement.png"),
    );
    assert.equal(await page.locator(".pack-stage").count(), 1);
    await page.evaluate(() => {
        window.packStatus = 404;
        window.streams[0].dispatchEvent(new Event("open"));
    });
    await page.waitForFunction(() => !document.querySelector(".screen-pack"));
    assert.equal(await page.locator("[data-ui-state=ready]").count(), 1);
    await page.evaluate(() => {
        window.packStatus = 200;
    });
    await reload(page, 3);
    await page.locator(".screen-pack").waitFor();
    await page.evaluate(() =>
        document
            .querySelector(".pack-stage img")
            .dispatchEvent(new Event("error")),
    );
    assert.equal(await page.locator(".screen-pack").count(), 0);
    await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
    const count = await page.evaluate(() => window.packRequests.length);
    await reload(page, 4);
    assert.equal(await page.evaluate(() => window.packRequests.length), count);
});
