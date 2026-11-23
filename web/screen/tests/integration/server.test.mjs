import { test, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { once } from "node:events";
import { readFile } from "node:fs/promises";
import { chromium } from "playwright";

// One prebuilt process/browser for this integration file, isolated page contexts.
const executable = new URL(
    `../../../../target/debug/examples/screen_fixture`,
    import.meta.url,
);
const fixture = spawn(fileURLToPath(executable), [], {
    stdio: ["pipe", "pipe", "pipe"],
});
const output = [];
const waiters = [];
let diagnostic = "";
fixture.stderr.on("data", (bytes) => {
    diagnostic += bytes;
});
createInterface({ input: fixture.stdout }).on("line", (line) => {
    const value = JSON.parse(line);
    const waiter = waiters.shift();
    if (waiter) waiter(value);
    else output.push(value);
});
const next = () =>
    output.length
        ? Promise.resolve(output.shift())
        : new Promise((resolve) => waiters.push(resolve));
const command = (value) => fixture.stdin.write(JSON.stringify(value) + "\n");
const token = "synthetic-screen-pair";
command({ start: { token, port: 0 } });
const { ready } = await next();
assert.equal(ready.host, "127.0.0.1");
const origin = `http://127.0.0.1:${ready.port}`;
const browser = await chromium.launch({
    headless: true,
    args: ["--disable-background-networking", "--no-proxy-server"],
});
after(async () => {
    await browser.close();
    const exited = once(fixture, "exit");
    command({ stop: true });
    assert.deepEqual(await next(), { stopped: true });
    assert.deepEqual(await exited, [0, null]);
    assert.equal(diagnostic, "");
});

const pairedUrl = `${origin}/?${new URLSearchParams({
    events: `/events?token=${token}`,
    actions: `/actions?token=${token}`,
})}`;

function chromeState(page, state) {
    return page.locator(`.screen-band.band-${state}`);
}

async function motion(band, name, duration, iterations = 1) {
    const actual = await band.evaluate((element) => {
        const [animation] = element.getAnimations({ subtree: true });
        const timing = animation.effect.getTiming();
        return [animation.animationName, timing.duration, timing.iterations];
    });
    assert.deepEqual(actual, [name, duration, iterations]);
}

test("B01–B07: the real paired stream paints stock chrome only on an opted-in page", async (t) => {
    const context = await browser.newContext({
        viewport: { width: 1280, height: 800 },
        serviceWorkers: "block",
    });
    t.after(() => context.close());
    const page = await context.newPage();
    await page.goto(pairedUrl);
    assert.equal(await page.locator(".screen-grip, .screen-band").count(), 0);
    await page.locator("html").evaluate((node) => {
        node.dataset.echoScreenChrome = "page";
    });
    const waiting = page
        .getByRole("status")
        .filter({ hasText: "Waiting for bridge" });
    await waiting.waitFor();
    const grip = page.locator(".screen-grip");
    assert.deepEqual(await grip.boundingBox(), {
        x: 0,
        y: 0,
        width: 1280,
        height: 16,
    });
    const state = async (next) => {
        command({ state: next });
        await page.locator(`[data-ui-state="${next.state}"]`).waitFor();
    };
    await state({ state: "listening" });
    const listening = chromeState(page, "listening");
    assert.match(
        await listening.evaluate(
            (node) => getComputedStyle(node).backgroundImage,
        ),
        /rgb\(0, 0, 255\).*rgb\(0, 255, 255\)/,
    );
    await motion(listening, "band-listening", 500);
    await state({ state: "thinking" });
    const thinking = chromeState(page, "thinking");
    await motion(thinking, "band-thinking", 200, Infinity);
    assert.deepEqual(
        await thinking.evaluate((node) => {
            const [animation] = node.getAnimations({ subtree: true });
            animation.pause();
            return [0, 99, 100, 199].map((ms) => {
                animation.currentTime = ms;
                return getComputedStyle(node, "::after").opacity;
            });
        }),
        ["0", "0", "1", "1"],
    );
    await state({ state: "speaking" });
    const speaking = chromeState(page, "speaking");
    await motion(speaking, "band-speaking", 1690, Infinity);
    assert.deepEqual(await sample(speaking, [0, 845]), [
        "rgb(0, 255, 255)",
        "rgb(0, 0, 255)",
    ]);
    await page.evaluate(() => {
        window.mutedPaint = new Promise((resolve) => {
            const band = document.querySelector(".screen-band");
            const observer = new MutationObserver(() => {
                if (!band.classList.contains("band-muted")) return;
                observer.disconnect();
                requestAnimationFrame(() =>
                    resolve([
                        getComputedStyle(band).backgroundColor,
                        band
                            .getAnimations()
                            .map((animation) => animation.animationName),
                    ]),
                );
            });
            observer.observe(band, { attributeFilter: ["class"] });
        });
    });
    await state({ state: "muted" });
    assert.deepEqual(await page.evaluate(() => window.mutedPaint), [
        "rgb(255, 0, 0)",
        ["band-muted"],
    ]);
    const muted = chromeState(page, "muted");
    await motion(muted, "band-muted", 700);
    assert.deepEqual(await sample(muted, [0, 700]), [
        "rgb(255, 0, 0)",
        "rgb(153, 0, 0)",
    ]);
    await state({ state: "error", message: "Local link failed", relink: true });
    const error = chromeState(page, "error");
    await motion(error, "band-error", 1100, 3);
    assert.deepEqual(await sample(error, [0, 550, 1650, 2750, 3300]), [
        "rgb(0, 0, 0)",
        "rgb(230, 0, 0)",
        "rgb(230, 0, 0)",
        "rgb(230, 0, 0)",
        "rgb(0, 0, 0)",
    ]);
    assert.equal(await page.getByText("Local link failed").count(), 1);
    assert.equal(await page.getByRole("button", { name: "Relink" }).count(), 1);
    await state({ state: "ready", message: "Echo Live ready" });
    assert.equal(
        await page.locator(".screen-band").getAttribute("class"),
        "screen-band",
    );
    assert.equal(await waiting.count(), 0);
    await page
        .locator("html")
        .evaluate((node) => delete node.dataset.echoScreenChrome);
    await state({ state: "speaking" });
    assert.equal(await page.locator(".screen-grip, .screen-band").count(), 0);
    console.log(
        "A2 paired states: 500ms listening, 100ms thinking steps, 1690ms speaking, first-frame red and 700ms mute, 3 × 1100ms error, dark Ready, zero legacy strips",
    );
});

function sample(band, times) {
    return band.evaluate((node, ms) => {
        const [animation] = node.getAnimations();
        animation.pause();
        return ms.map((time) => {
            animation.currentTime = time;
            return getComputedStyle(node).backgroundColor;
        });
    }, times);
}

test("B08: a silent established connection enters waiting within 5 seconds and the real stream recovers", async (t) => {
    const context = await browser.newContext({ serviceWorkers: "block" });
    context.setDefaultTimeout(5000);
    t.after(() => context.close());
    const page = await context.newPage();
    await page.clock.install({ time: 0 });
    await page.clock.pauseAt(0);
    let unreachable = false;
    const heldCheck = Promise.withResolvers();
    const heldStream = Promise.withResolvers();
    let checks = 0;
    await context.route("**/*", (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin !== origin) return route.abort();
        if (url.pathname === "/" && !request.isNavigationRequest()) {
            checks++;
            if (unreachable) {
                heldCheck.resolve(route);
                return;
            }
        }
        if (url.pathname === "/events" && unreachable) {
            heldStream.resolve(route);
            return;
        }
        return route.continue();
    });
    const initialProbe = page.waitForResponse(
        (response) => response.url() === `${origin}/`,
    );
    command({ state: { state: "speaking" } });
    await page.goto(pairedUrl);
    await initialProbe;
    await page.locator('[data-ui-state="speaking"]').waitFor();
    await page.locator("html").evaluate((node) => {
        node.dataset.echoScreenChrome = "page";
    });
    await chromeState(page, "speaking").waitFor();
    const waiting = page
        .getByRole("status")
        .filter({ hasText: "Waiting for bridge" });
    unreachable = true;
    await page.clock.runFor(2000);
    const probe = await heldCheck.promise;
    await page.clock.runFor(2999);
    assert.equal(await waiting.count(), 0);
    assert.equal(await chromeState(page, "speaking").count(), 1);
    await page.clock.runFor(1);
    await waiting.waitFor();
    assert.equal(
        await page.locator(".screen-band").getAttribute("class"),
        "screen-band",
    );
    assert.equal(checks, 2);
    const reconnect = await heldStream.promise;
    unreachable = false;
    await probe.abort().catch(() => {});
    command({ state: { state: "listening" } });
    await reconnect.continue();
    await chromeState(page, "listening").waitFor();
    assert.equal(await waiting.count(), 0);
    assert.equal(await page.locator(".screen-grip").count(), 1);
    await page.evaluate(() => window.dispatchEvent(new Event("offline")));
    await waiting.waitFor();
    command({ state: { state: "muted" } });
    await chromeState(page, "muted").waitFor();
    assert.equal(await waiting.count(), 0);
    console.log(
        "A2 outage: silent stream stays active through 4999ms, waiting at 5000ms, first valid SSE restores chrome, offline signal clears and recovers",
    );
});

test("F02/F14/F15/F16: native SSE/page/actions, keyboard focus, exact assets and revoked refusal", async (t) => {
    const context = await browser.newContext({
        viewport: { width: 1280, height: 800 },
        serviceWorkers: "block",
    });
    t.after(() => context.close());
    await context.route("**/*", (route) =>
        new URL(route.request().url()).origin === origin
            ? route.continue()
            : route.abort(),
    );
    const query = new URLSearchParams({
        events: `/events?token=${token}`,
        actions: `/actions?token=${token}`,
    });
    const page = await context.newPage();
    await page.goto(`${origin}/?${query}`);
    const update = async (state) => {
        command({ state });
        await page.locator(`[data-ui-state="${state.state}"]`).waitFor();
    };
    await update({ state: "ready", message: "Echo Live ready" });
    assert.equal(
        await page.getByText(/^(ready|echo live ready|start)$/i).count(),
        0,
    );
    await page
        .getByRole("button", { name: "Begin session", exact: true })
        .click();
    assert.deepEqual(await next(), {
        action: { action: "tap", target: "primary" },
    });
    const approval = {
        state: "approval",
        id: "approval-1",
        prompt: "Allow local action?",
        accept_label: "Allow",
        deny_label: "Deny",
    };
    await update(approval);
    assert.equal(
        await page.locator("section p").textContent(),
        approval.prompt,
    );
    await page.keyboard.press("Tab");
    assert.equal(await page.locator(":focus").textContent(), "Allow");
    await page.keyboard.press("Tab");
    assert.equal(await page.locator(":focus").textContent(), "Deny");
    await page.keyboard.press("Shift+Tab");
    await page.keyboard.press("Enter");
    assert.deepEqual(await next(), {
        action: { action: "approve", id: "approval-1" },
    });
    // An accepted action re-enables the controls and writes no status text.
    await page.locator("button:focus:enabled").waitFor();
    assert.equal(await page.getByRole("status").textContent(), "");
    assert.equal(await page.locator(":focus").textContent(), "Allow");
    await page.keyboard.press("Tab");
    await page.keyboard.press("Space");
    assert.deepEqual(await next(), {
        action: { action: "deny", id: "approval-1" },
    });
    for (const button of await page.getByRole("button").all()) {
        const box = await button.boundingBox();
        assert.ok(
            box.x >= 0 &&
                box.y >= 0 &&
                box.width >= 64 &&
                box.height >= 64 &&
                box.x + box.width <= 1280 &&
                box.y + box.height <= 800,
        );
    }
    await update({
        state: "error",
        message: "Local link failed",
        relink: true,
    });
    assert.equal(
        await page.locator("section p").textContent(),
        "Local link failed",
    );
    await update(approval);
    // The document names the page build; every request names the one the
    // running page loaded with, so this page never reloads itself.
    const build = await page
        .locator('meta[name="echo-build"]')
        .getAttribute("content");
    assert.match(build, /^[0-9a-f]{16}$/);
    for (const [path, file] of [
        ["/", "index.html"],
        ["/tokens.css", "tokens.css"],
        ["/screen.css", "screen.css"],
        ["/screen.js", "screen.js"],
    ]) {
        const response = await fetch(origin + path);
        assert.equal(response.status, 200);
        const source = await readFile(
            new URL(`../../dist/${file}`, import.meta.url),
            "utf8",
        );
        assert.equal(
            await response.text(),
            path === "/"
                ? source.replace(
                      "<head>",
                      `<head>\n    <meta name="echo-build" content="${build}" />`,
                  )
                : source,
        );
        assert.equal(response.headers.get("referrer-policy"), "no-referrer");
    }
    // The one bundled font is served byte for byte; nothing else under /fonts.
    const font = "/fonts/manrope-latin-wght-normal.woff2";
    const served = await fetch(origin + font);
    assert.equal(served.status, 200);
    assert.equal(served.headers.get("content-type"), "font/woff2");
    assert.equal(
        Buffer.from(await served.arrayBuffer()).equals(
            await readFile(new URL(`../../dist${font}`, import.meta.url)),
        ),
        true,
    );
    assert.equal((await fetch(origin + "/fonts/OFL.txt")).status, 404);
    // The served page resolves the token properties and loads Manrope.
    assert.deepEqual(
        await page.evaluate(async () => {
            const style = getComputedStyle(document.documentElement);
            const faces = await document.fonts.load("16px Manrope");
            return {
                primary: style.getPropertyValue("--color-primary").trim(),
                font: style.getPropertyValue("--font-sans").trim(),
                faces: faces.map((face) => `${face.family} ${face.status}`),
            };
        }),
        {
            primary: "#ffda36",
            font: "Manrope, system-ui, sans-serif",
            faces: ["Manrope loaded"],
        },
    );
    command({ revoke: true });
    assert.deepEqual(await next(), { revoked: true });
    const posts = [];
    page.on("request", (request) => {
        if (request.method() === "POST") posts.push(request.postDataJSON());
    });
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    await page
        .getByRole("status")
        .filter({ hasText: "Action could not be sent" })
        .waitFor();
    assert.equal(
        await page.locator("section p").textContent(),
        approval.prompt,
    );
    assert.equal(
        await page
            .getByRole("button", { name: "Allow", exact: true })
            .isEnabled(),
        true,
    );
    assert.deepEqual(posts, [{ action: "approve", id: "approval-1" }]);
    // The failure report goes away without a newer frame. The revoked
    // stream may report its own interruption in the same line meanwhile.
    await page.waitForFunction(
        () =>
            document.querySelector('[role="status"]').textContent !==
            "Action could not be sent",
        undefined,
        { timeout: 8000 },
    );
    assert.equal(
        await page.locator("section p").textContent(),
        approval.prompt,
    );
    assert.deepEqual(output, []);
    console.log(
        "native page: exact Ready/Error/Approval; pointer Tap; Enter Approve; Space Deny; focus/1280x800 controls; revoked POST refused once with no action",
    );
});
