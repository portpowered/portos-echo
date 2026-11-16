import { chromium } from "playwright";
import { after } from "node:test";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import assert from "node:assert/strict";

const browser = await chromium.launch({
    headless: true,
    args: [
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-domain-reliability",
        "--no-proxy-server",
    ],
});
after(() => browser.close());
const html = await readFile(
    new URL("../dist/index.html", import.meta.url),
    "utf8",
);
const css = await readFile(
    new URL("../dist/screen.css", import.meta.url),
    "utf8",
);
const js = await readFile(
    new URL("../dist/screen.js", import.meta.url),
    "utf8",
);
export async function pageFor(
    t,
    query = "?events=%2Fevents&actions=%2Factions",
) {
    const context = await browser.newContext({
        viewport: { width: 1280, height: 800 },
        serviceWorkers: "block",
    });
    let page;
    let coverageStarted = false;
    const requests = [];
    t.after(async () => {
        try {
            if (!coverageStarted) return;
            assert.deepEqual(
                requests,
                [],
                "component must not request any network resource",
            );
            const coverage = await page.coverage.stopJSCoverage();
            await mkdir("test-results/coverage", { recursive: true });
            await writeFile(
                `test-results/coverage/${randomUUID()}.json`,
                JSON.stringify(
                    coverage.filter((item) => item.url === "screen.js"),
                ),
            );
        } finally {
            await context.close();
        }
    });
    // Record both clock operations before any page can run the installed clock.
    await context.clock.install({ time: 0 });
    await context.clock.pauseAt(0);
    await context.route("**/*", (route) => {
        requests.push(route.request().url());
        return route.abort();
    });
    page = await context.newPage();
    await page.coverage.startJSCoverage({ resetOnNavigation: false });
    coverageStarted = true;
    await page.setContent(
        html
            .replace('<link rel="stylesheet" href="screen.css" />', "")
            .replace('<script src="screen.js"></script>', "")
            .replace(
                "<head>",
                `<head><base href="http://127.0.0.1:4173/${query.replaceAll("&", "&amp;")}">`,
            ),
    );
    await page.addStyleTag({ content: css });
    await page.evaluate(() => {
        window.calls = [];
        window.streams = [];
        window.postStatus = 204;
        window.timers = new Set();
        const schedule = window.setTimeout.bind(window);
        const cancel = window.clearTimeout.bind(window);
        window.setTimeout = (callback, delay) => {
            const id = schedule(() => {
                window.timers.delete(id);
                callback();
            }, delay);
            window.timers.add(id);
            return id;
        };
        window.clearTimeout = (id) => {
            window.timers.delete(id);
            cancel(id);
        };
        window.aborted = 0;
        window.EventSource = class {
            constructor(url) {
                this.url = url;
                this.closed = false;
                window.streams.push(this);
            }
            close() {
                this.closed = true;
            }
        };
        window.fetch = (url, options) => {
            options.signal.addEventListener("abort", () => window.aborted++);
            window.calls.push({
                url,
                method: options.method,
                headers: options.headers,
                body: options.body,
            });
            if (window.postStatus === "reject")
                return Promise.reject(new Error("controlled"));
            if (window.postStatus === "pending")
                return new Promise((resolve) => {
                    window.resolvePost = resolve;
                });
            return Promise.resolve({ status: window.postStatus });
        };
    });
    await page.addScriptTag({ content: `${js}\n//# sourceURL=screen.js` });
    await page.evaluate(() => document.fonts.ready);
    return page;
}
export const emit = (page, wire) =>
    page.evaluate((data) => window.streams[0].onmessage({ data }), wire);
export const calls = (page) => page.evaluate(() => window.calls);
export async function bounds(page, scrollText = false) {
    const contrasts = await page.evaluate(() => {
        function luminance(color) {
            const rgb = color
                .match(/\d+/g)
                .slice(0, 3)
                .map(Number)
                .map((n) => n / 255)
                .map((v) =>
                    v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4,
                );
            return 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        }
        return [...document.querySelectorAll("h1,p,button")]
            .filter((node) => node.textContent)
            .map((node) => {
                let background = node;
                while (
                    getComputedStyle(background).backgroundColor ===
                        "rgba(0, 0, 0, 0)" &&
                    background.parentElement
                )
                    background = background.parentElement;
                const a = luminance(getComputedStyle(node).color);
                const b = luminance(
                    getComputedStyle(background).backgroundColor,
                );
                return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
            });
    });
    for (const ratio of contrasts)
        assert.ok(ratio >= 4.5, `text contrast ${ratio}`);
    const measurements = await page.evaluate(() =>
        [...document.querySelectorAll("h1, p, button")]
            .filter((node) => node.textContent)
            .map((node) => {
                const box = node.getBoundingClientRect();
                let clipped = false;
                for (
                    let parent = node.parentElement;
                    parent;
                    parent = parent.parentElement
                ) {
                    if (getComputedStyle(parent).overflowY !== "visible") {
                        const area = parent.getBoundingClientRect();
                        clipped ||=
                            box.top < area.top || box.bottom > area.bottom;
                    }
                }
                return {
                    tag: node.tagName,
                    x: box.x,
                    y: box.y,
                    width: box.width,
                    height: box.height,
                    scroll: node.scrollWidth,
                    client: node.clientWidth,
                    clipped,
                };
            }),
    );
    for (const box of measurements) {
        if (!scrollText || box.tag !== "P")
            assert.equal(box.clipped, false, JSON.stringify(box));
        if (!scrollText || box.tag !== "P")
            assert.ok(
                box.x >= 0 &&
                    box.y >= 0 &&
                    box.x + box.width <= 1280 &&
                    box.y + box.height <= 800,
                JSON.stringify(box),
            );
        assert.ok(box.scroll <= box.client, JSON.stringify(box));
        if (box.tag === "BUTTON")
            assert.ok(box.width >= 64 && box.height >= 64);
    }
    if (scrollText) {
        const area = await page.locator(".details").boundingBox();
        assert.ok(
            area.x >= 0 &&
                area.y >= 0 &&
                area.x + area.width <= 1280 &&
                area.y + area.height <= 800,
        );
        assert.equal(
            await page
                .locator(".details")
                .evaluate((node) => getComputedStyle(node).overflowY),
            "auto",
        );
    }
    const buttons = measurements.filter((box) => box.tag === "BUTTON");
    for (const [index, a] of buttons.entries())
        for (const b of buttons.slice(index + 1))
            assert.ok(
                a.x + a.width <= b.x ||
                    b.x + b.width <= a.x ||
                    a.y + a.height <= b.y ||
                    b.y + b.height <= a.y,
                "touch targets must not overlap",
            );
    assert.equal(
        await page.evaluate(() => document.documentElement.scrollHeight),
        800,
    );
    assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth),
        1280,
    );
    console.log(
        `geometry: ${measurements.length} text/control boxes; ${buttons.length} targets >=64px; min contrast ${Math.min(...contrasts).toFixed(2)}:1; viewport 1280x800`,
    );
}
