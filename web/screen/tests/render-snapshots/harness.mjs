// Render harness: the built page in headless Chromium at 960x600, served from
// a synthetic loopback origin with real generated pack media. Nothing leaves
// the process; every other request is aborted.
import { after } from "node:test";
import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { extname, join } from "node:path";
import { chromium } from "playwright";

export const width = 960;
export const height = 600;
export const out = "test-results/screenshots/render";
const origin = "http://127.0.0.1:4173";
const dist = new URL("../../dist/", import.meta.url);
const types = {
    ".css": "text/css",
    ".js": "text/javascript",
    ".png": "image/png",
    ".webm": "video/webm",
    ".mp4": "video/mp4",
};
export const browser = await chromium.launch({
    headless: true,
    args: ["--disable-background-networking", "--no-proxy-server"],
});
after(() => browser.close());

const index = (await readFile(new URL("index.html", dist), "utf8")).replace(
    "<head>",
    '<head><meta name="echo-packs" content="v1"><meta name="echo-settings" content="v1">',
);

function catalog(pack) {
    const installed = pack
        ? [
              {
                  id: pack.id,
                  name: pack.id.split("@")[0],
                  version: pack.id.split("@")[1],
                  rest_url: `/screen-packs/v1/${pack.id}/rest.png`,
              },
          ]
        : [];
    return {
        version: 1,
        revision: 1,
        active: pack?.id ?? null,
        warning: null,
        installed,
        jobs: [],
    };
}

// Media seeks need byte ranges; answer them like the screen server does.
function ranged(route, body, contentType) {
    const range = /bytes=(\d+)-(\d*)/.exec(route.request().headers().range);
    if (!range) return route.fulfill({ body, contentType });
    const start = Number(range[1]);
    const end = range[2] ? Number(range[2]) : body.length - 1;
    return route.fulfill({
        status: 206,
        contentType,
        headers: {
            "accept-ranges": "bytes",
            "content-range": `bytes ${start}-${end}/${body.length}`,
        },
        body: body.subarray(start, end + 1),
    });
}

async function serve(route, { pack, settings }) {
    const url = new URL(route.request().url());
    if (url.origin !== origin) return route.abort();
    const path = decodeURIComponent(url.pathname);
    const json = (value) => route.fulfill({ json: value });
    if (path === "/")
        return route.fulfill({ body: index, contentType: "text/html" });
    if (path === "/screen.css" || path === "/screen.js")
        return route.fulfill({
            body: await readFile(new URL(path.slice(1), dist)),
            contentType: types[extname(path)],
        });
    if (path === "/screen-packs/v1") return json(catalog(pack));
    const asset = /^\/screen-packs\/v1\/([^/]+)\/([a-z0-9_.-]+)$/.exec(path);
    if (pack && asset?.[1] === pack.id) {
        if (asset[2] === "pack.json") return json(pack.manifest);
        const body = await readFile(join(pack.dir, asset[2]));
        return ranged(route, body, types[extname(asset[2])]);
    }
    if (path === "/settings/v2" && settings) return json(settings.body);
    if (path === "/settings/v2/openai" && settings?.openAI)
        return json(settings.openAI);
    return route.fulfill({ status: 404, body: "" });
}

export async function openScreen(t, { pack, settings } = {}) {
    const context = await browser.newContext({
        viewport: { width, height },
        locale: "en-US",
        timezoneId: "UTC",
        hasTouch: true,
        serviceWorkers: "block",
    });
    t.after(() => context.close());
    // Date is fixed for the clock; timers and media keep running.
    await context.clock.setFixedTime(new Date("2026-11-21T09:41:00Z"));
    await context.addInitScript(() => {
        window.streams = [];
        window.EventSource = class extends EventTarget {
            constructor(url) {
                super();
                this.url = url;
                window.streams.push(this);
            }
            close() {}
        };
    });
    await context.route("**/*", (route) => serve(route, { pack, settings }));
    const page = await context.newPage();
    await page.goto(
        `${origin}/?events=%2Fevents%3Ftoken%3Drender&actions=%2Factions%3Ftoken%3Drender`,
    );
    await page.evaluate(() => document.fonts.ready);
    return page;
}

let seq = 0;
export const emit = (page, state) =>
    page.evaluate(
        (data) => window.streams[0].onmessage({ data }),
        JSON.stringify({ seq: ++seq, state }),
    );

// Wait until `clip` owns the stage, finish any glide, then hold one frame.
export async function hold(page, clip, frame = 0) {
    await page.waitForFunction(
        (clip) =>
            [
                ...document.querySelectorAll(
                    ".pack-stage video:not([hidden]):not([data-leaving])",
                ),
            ].some(
                (video) =>
                    new URL(video.src).pathname.endsWith(`/${clip}`) &&
                    video.readyState >= 2,
            ),
        clip,
    );
    await page.evaluate(
        async (time) => {
            for (const animation of document.getAnimations())
                animation.finish();
            const video = document.querySelector(
                ".pack-stage video:not([hidden]):not([data-leaving])",
            );
            video.pause();
            await new Promise((resolve) => {
                video.addEventListener("seeked", resolve, { once: true });
                video.currentTime = time;
            });
            for (let i = 0; i < 3; i++)
                await new Promise((resolve) => requestAnimationFrame(resolve));
        },
        (frame + 0.5) / 30,
    );
}

// Bounding boxes, in CSS px, of the bright face pixels of the shown frame:
// one box per eye (split at the stage's centre line) plus the whole face.
export function faceBoxes(page) {
    return page.evaluate(() => {
        const video = document.querySelector(
            ".pack-stage video:not([hidden]):not([data-leaving])",
        );
        const stage = video.getBoundingClientRect();
        const scale = Math.min(
            stage.width / video.videoWidth,
            stage.height / video.videoHeight,
        );
        const w = Math.round(video.videoWidth * scale);
        const h = Math.round(video.videoHeight * scale);
        const left = stage.left + (stage.width - w) / 2;
        const top = stage.top + (stage.height - h) / 2;
        const canvas = new OffscreenCanvas(w, h);
        const context = canvas.getContext("2d");
        context.drawImage(video, 0, 0, w, h);
        const pixels = context.getImageData(0, 0, w, h).data;
        const box = () => ({ x0: Infinity, y0: Infinity, x1: -1, y1: -1 });
        const boxes = { left: box(), right: box(), face: box() };
        for (let y = 0; y < h; y++)
            for (let x = 0; x < w; x++) {
                const i = (y * w + x) * 4;
                const bright = Math.max(
                    pixels[i],
                    pixels[i + 1],
                    pixels[i + 2],
                );
                if (pixels[i + 3] < 128 || bright < 128) continue;
                for (const b of [
                    boxes.face,
                    x < w / 2 ? boxes.left : boxes.right,
                ]) {
                    b.x0 = Math.min(b.x0, x);
                    b.y0 = Math.min(b.y0, y);
                    b.x1 = Math.max(b.x1, x);
                    b.y1 = Math.max(b.y1, y);
                }
            }
        return Object.fromEntries(
            Object.entries(boxes).map(([name, b]) => [
                name,
                {
                    x: left + b.x0,
                    y: top + b.y0,
                    width: b.x1 - b.x0 + 1,
                    height: b.y1 - b.y0 + 1,
                },
            ]),
        );
    });
}

export const intersects = (a, b) =>
    a.x < b.x + b.width &&
    b.x < a.x + a.width &&
    a.y < b.y + b.height &&
    b.y < a.y + a.height;

// Every visible native surface: gear, clock, date, heading, card, feedback.
export async function surfaces(page) {
    const found = {};
    for (const [name, selector] of [
        ["gear", ".corner-control"],
        ["clock", ".pack-clock"],
        ["date", ".pack-date"],
        ["heading", ".screen-pack > h1"],
        ["card", ".screen-pack > .details"],
        ["controls", ".screen-pack > .controls"],
        ["feedback", ".screen-pack + [role=status]"],
    ]) {
        const node = page.locator(selector);
        if ((await node.count()) && (await node.first().isVisible())) {
            const box = await node.first().boundingBox();
            if (box && box.width && box.height) found[name] = box;
        }
    }
    return found;
}

export async function shoot(page, name) {
    await mkdir(out, { recursive: true });
    const png = await page.screenshot({ path: `${out}/${name}.png` });
    assert.equal(png.length > 0, true);
    return png;
}

export async function writeOut(name, bytes) {
    await mkdir(out, { recursive: true });
    await writeFile(`${out}/${name}`, bytes);
}
