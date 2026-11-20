import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { once } from "node:events";
import { createHash } from "node:crypto";
import { cp, mkdtemp, mkdir, readFile, rm, stat, utimes } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { archivePeer } from "./pack-peer.mjs";
import { concurrentResources } from "./pack-resources.mjs";

const token = "synthetic-pack-pair";
const id = "template@0.1.0";
const generated = process.env.SCREEN_PACK_FIXTURE;
assert.ok(generated, "suite owner must prepare generated assets");

async function open(t, { prepare, beforeNavigate, initialState, archivePath, viewport, locale } = {}) {
    const root = await mkdtemp(join(tmpdir(), "screen-pack-journey-"));
    const home = join(root, "home");
    await mkdir(home);
    await prepare?.(home, generated);
    const archive = await readFile(
        archivePath ?? join(generated, "template-0.1.0.screenpack.tar.gz"),
    );
    const sha256 = createHash("sha256").update(archive).digest("hex");
    const peer = await archivePeer(root, archive);
    const child = spawn(
        fileURLToPath(
            new URL(
                "../../../../target/debug/examples/screen_pack_fixture",
                import.meta.url,
            ),
        ),
        [],
        {
            stdio: ["pipe", "pipe", "pipe"],
        },
    );
    let diagnostic = "";
    child.stderr.on("data", (bytes) => {
        diagnostic += bytes;
    });
    const lines = createInterface({ input: child.stdout })[
        Symbol.asyncIterator
    ]();
    const next = async () => JSON.parse((await lines.next()).value);
    const command = (value) => child.stdin.write(JSON.stringify(value) + "\n");
    const exited = once(child, "exit");
    const browser = await chromium.launch({
        headless: true,
        args: ["--disable-background-networking", "--no-proxy-server"],
    });
    t.after(async () => {
        try {
            for (const context of browser.contexts()) {
                for (const page of context.pages()) {
                    console.log(
                        "F25 final surface:",
                        await page
                            .locator(".settings-panel [role=status]")
                            .textContent(),
                    );
                    console.log(
                        "F25 final media:",
                        await page.locator("video").evaluateAll((videos) =>
                            videos.map((video) => ({
                                file: video.src
                                    ? new URL(video.src).pathname
                                          .split("/")
                                          .at(-1)
                                    : null,
                                hidden: video.hidden,
                                ready: video.readyState,
                                frames: video.getVideoPlaybackQuality()
                                    .totalVideoFrames,
                            })),
                        ),
                    );
                    await mkdir("test-results/screenshots", {
                        recursive: true,
                    });
                    await page.screenshot({
                        path: "test-results/screenshots/generated-pack-final.png",
                    });
                }
            }
            await browser.close();
            command({ stop: true });
            assert.deepEqual(await next(), {
                stopped: true,
                restart_requested: false,
            });
            assert.deepEqual(await exited, [0, null]);
            assert.equal(diagnostic, "");
        } finally {
            await browser.close();
            child.kill();
            await peer.close();
            await rm(root, { recursive: true, force: true });
        }
    });
    command({ home, ca: join(root, "ca.pem") });
    const { ready } = await next();
    if (initialState) {
        command({ state: initialState });
        assert.deepEqual(await next(), { sent: true });
    }
    const origin = `http://127.0.0.1:${ready}`;
    const context = await browser.newContext({
        viewport: viewport ?? { width: 1280, height: 800 },
        locale,
        serviceWorkers: "block",
    });
    const outside = [];
    await context.route("**/*", (route) => {
        if (new URL(route.request().url()).origin === origin)
            return route.continue();
        outside.push(route.request().url());
        return route.abort();
    });
    await context.addInitScript(() => {
        window.packReloads = [];
        window.screenFrames = [];
        const Native = window.EventSource;
        window.EventSource = class extends Native {
            constructor(...args) {
                super(...args);
                this.addEventListener("screen-pack-reload", (event) =>
                    window.packReloads.push(JSON.parse(event.data)),
                );
                this.addEventListener("message", (event) =>
                    window.screenFrames.push(JSON.parse(event.data)),
                );
            }
        };
    });
    const page = await context.newPage();
    context.on("response", (response) => {
        if (response.status() >= 400)
            console.log(
                "F25 refused response:",
                response.status(),
                new URL(response.url()).pathname,
            );
    });
    page.setDefaultTimeout(5000);
    await beforeNavigate?.(page);
    const query = new URLSearchParams({
        events: `/events?token=${token}`,
        actions: `/actions?token=${token}`,
    });
    await page.goto(`${origin}/?${query}`);
    const update = async (state) => {
        command({ state });
        assert.deepEqual(await next(), { sent: true });
        await page
            .getByRole("heading", { name: state.state.replaceAll("_", " "), exact: true })
            .waitFor();
    };
    const api = async (path) => {
        const response = await fetch(`${origin}${path}?token=${token}`);
        assert.equal(response.status, 200);
        return response.json();
    };
    return { page, context, home, peer, sha256, update, api, outside, origin, next };
}

test("R1 native CSS, generated character and multiple previews retain selection", async (t) => {
    await concurrentResources(t, open, playing, refresh);
});

async function playing(page, name) {
    await page.waitForFunction((name) => {
        const video = document.querySelector("video:not([hidden])");
        return (
            video &&
            new URL(video.src).pathname.endsWith(`/${name}`) &&
            video.readyState >= 2 &&
            video.getVideoPlaybackQuality().totalVideoFrames > 0
        );
    }, name);
    return page
        .locator("video:not([hidden])")
        .evaluate((node) => new URL(node.src).pathname.split("/").at(-1));
}

async function reloads(page, revision) {
    await page.waitForFunction(
        (revision) => window.packReloads.at(-1)?.revision === revision,
        revision,
    );
    assert.deepEqual(
        await page.evaluate(() => window.packReloads.map((r) => r.revision)),
        Array.from({ length: revision }, (_, i) => i + 1),
    );
}

async function refresh(page) {
    await page
        .getByRole("button", { name: /^(Load screen packs|Refresh packs)$/ })
        .click();
    await page.getByText("Screen packs loaded", { exact: true }).waitFor();
}

test("F25 generated template: production TLS install/writer/SSE, real media and hot rollback", async (t) => {
    const f = await open(t);
    const { page, home } = f;
    const navigations = [];
    page.on("framenavigated", (frame) => {
        if (frame === page.mainFrame()) navigations.push(frame.url());
    });
    await f.update({ state: "ready", message: "Ready" });
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await refresh(page);
    assert.equal(
        await page
            .getByText("No screen packs installed", { exact: true })
            .count(),
        1,
    );
    await page.getByLabel("Pack HTTPS URL", { exact: true }).fill(f.peer.url);
    await page.getByLabel("Pack SHA-256", { exact: true }).fill(f.sha256);
    await page
        .getByRole("button", { name: "Download pack", exact: true })
        .click();
    await reloads(page, 1);
    await refresh(page);
    const preview = page.getByRole("img", { name: "template rest pose" });
    await preview.waitFor();
    await preview.scrollIntoViewIfNeeded();
    await preview.evaluate((img) => img.decode());
    assert.equal(
        await preview.evaluate(
            (img) => img.complete && img.naturalWidth === 640,
        ),
        true,
    );
    await page.getByLabel("Screen pack", { exact: true }).selectOption("");
    await page
        .getByRole("button", { name: "Use screen pack", exact: true })
        .click();
    await reloads(page, 2);
    await page
        .getByText(
            "Screen pack saved. Appearance updates without restarting the bridge.",
            { exact: true },
        )
        .waitFor();
    await page.getByLabel("Screen pack", { exact: true }).selectOption(id);
    await page
        .getByRole("button", { name: "Use screen pack", exact: true })
        .click();
    await reloads(page, 3);
    await page
        .getByText(
            "Screen pack saved. Appearance updates without restarting the bridge.",
            { exact: true },
        )
        .waitFor();
    assert.equal(
        await readFile(join(home, "settings.toml"), "utf8"),
        `screen_pack = "${id}"\n`,
    );
    assert.equal((await stat(join(home, "settings.toml"))).mode & 0o777, 0o600);
    await page.getByRole("button", { name: "Back", exact: true }).click();
    const sources = [await playing(page, "idle.webm")];
    for (const state of ["listening", "thinking", "speaking", "ready"]) {
        await f.update({
            state,
            ...(state === "ready" ? { message: "Ready" } : {}),
        });
        sources.push(
            await playing(page, `${state === "ready" ? "idle" : state}.webm`),
        );
    }
    assert.deepEqual(sources, [
        "idle.webm",
        "listening.webm",
        "thinking.webm",
        "speaking.webm",
        "idle.webm",
    ]);
    console.log(`F25 decoded state sources: ${sources.join(" -> ")}`);
    await f.update({
        state: "emotion",
        name: "happy",
        frames: ["unused"],
        frame_ms: 50,
    });
    await playing(page, "happy.webm");
    await playing(page, "idle.webm");
    await f.update({
        state: "emotion",
        name: "unknown",
        frames: ["fallback-frame"],
        frame_ms: 50,
    });
    assert.equal(
        await page.getByLabel("Animation frame").textContent(),
        "fallback-frame",
    );
    await f.update({ state: "ready", message: "Ready" });
    await playing(page, "idle.webm");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await refresh(page);
    await page.getByLabel("Pack HTTPS URL", { exact: true }).fill(f.peer.url);
    await page.getByLabel("Pack SHA-256", { exact: true }).fill("0".repeat(64));
    await page
        .getByRole("button", { name: "Download pack", exact: true })
        .click();
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
    await page.getByText(/failed \(checksum mismatch\)/).waitFor();
    assert.equal((await f.api("/screen-packs/v1")).active, id);
    assert.equal((await f.api("/settings/v1")).restart_pending, false);
    const rest = join(home, "screens", id, "rest.png");
    assert.deepEqual(
        await readFile(rest),
        await readFile(join(generated, id, "rest.png")),
    );
    await page.getByRole("button", { name: "Back", exact: true }).click();
    const clip = join(home, "screens", id, "idle.webm");
    const metadata = await stat(clip);
    await utimes(clip, metadata.atime, new Date(metadata.mtimeMs + 1000));
    await reloads(page, 4);
    await playing(page, "idle.webm");
    await page.waitForFunction(() =>
        document
            .querySelector("video:not([hidden])")
            ?.src.includes("revision=4"),
    );
    await mkdir("test-results/screenshots", { recursive: true });
    for (const viewport of [
        { width: 1280, height: 800 },
        { width: 640, height: 600 },
    ]) {
        await page.setViewportSize(viewport);
        await page.screenshot({
            path: `test-results/screenshots/generated-pack-${viewport.width}x${viewport.height}.png`,
        });
    }
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.waitForFunction(
        () => !document.querySelector("video:not([hidden])"),
    );
    assert.equal(await page.locator(".pack-stage img").isVisible(), true);
    assert.equal(
        await page
            .locator(".pack-stage img")
            .evaluate((img) => img.naturalWidth),
        640,
    );
    // Observe the real decoder's MP4 path, overriding only capability selection.
    const mp4 = await f.context.newPage();
    mp4.setDefaultTimeout(5000);
    mp4.on("console", (message) =>
        console.log("F25 MP4 page:", message.text()),
    );
    console.log(
        "F25 real codec capabilities:",
        await mp4.evaluate(() => {
            const video = document.createElement("video");
            return {
                vp9: video.canPlayType('video/webm; codecs="vp9"'),
                h264: video.canPlayType('video/mp4; codecs="avc1.42E01E"'),
            };
        }),
    );
    await mp4.addInitScript(() => {
        HTMLMediaElement.prototype.canPlayType = () => "";
        window.addEventListener(
            "error",
            (event) => {
                if (event.target instanceof HTMLMediaElement)
                    console.log("media error", event.target.error?.code);
            },
            true,
        );
    });
    await mp4.goto(page.url());
    await playing(mp4, "idle.mp4");
    await mp4.close();
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await refresh(page);
    await page.getByLabel("Screen pack", { exact: true }).selectOption("");
    await page
        .getByRole("button", { name: "Use screen pack", exact: true })
        .click();
    await reloads(page, 5);
    await page
        .getByText(
            "Screen pack saved. Appearance updates without restarting the bridge.",
            { exact: true },
        )
        .waitFor();
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await page.waitForFunction(() => !document.querySelector(".screen-pack"));
    assert.equal(
        await readFile(join(home, "settings.toml"), "utf8"),
        'screen_pack = ""\n',
    );
    assert.equal((await f.api("/settings/v1")).restart_pending, false);
    assert.deepEqual(f.peer.requests, [
        "/template.screenpack.tar.gz",
        "/template.screenpack.tar.gz",
    ]);
    assert.deepEqual(f.outside, []);
    assert.deepEqual(navigations, []);
    const frames = await page.evaluate(() => window.screenFrames);
    assert.deepEqual(
        frames.map((frame) => frame.seq),
        Array.from({ length: 8 }, (_, i) => i + 1),
    );
    console.log(
        `F25 production: ${sources.join(" -> ")}; happy once -> idle; unknown -> frames; MP4 decoded; reloads=5; restart_requests=0; checksum refused; same page/server alive`,
    );
});

test("FACE candidate installs over the baseline and fits the paired 960x600 page", { skip: !process.env.FACE_CANDIDATE, timeout: 45000 }, async (t) => {
    const candidate = process.env.FACE_CANDIDATE;
    const latest = "template@0.2.0";
    const f = await open(t, {
        viewport: { width: 960, height: 600 },
        locale: "en-GB",
        archivePath: join(candidate, "template-0.2.0.screenpack.tar.gz"),
        prepare: async (home) => {
            await mkdir(join(home, "screens"));
            await cp(join(process.env.FACE_OLDER, id), join(home, "screens", id), { recursive: true });
        },
        initialState: { state: "ready", message: "Ready to help whenever you need me. Try asking about your timers, the weather, or a quick reminder." },
    });
    const { page } = f;
    const catalog = () => f.api("/screen-packs/v1");
    assert.equal((await catalog()).active, id);
    const status = await page.evaluate(async ({ url, sha256, token }) => (await fetch(`/screen-packs/v1/install?token=${token}`, {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ version: 1, url, sha256 }),
    })).status, { url: f.peer.url, sha256: f.sha256, token });
    assert.equal(status, 202);
    await reloads(page, 1);
    const installed = await catalog();
    assert.equal(installed.active, latest);
    assert.deepEqual(installed.installed.map((pack) => pack.id).sort(), [id, latest]);
    for (const [key, filename] of [["manifest_url", "pack.json"], ["rest_url", "rest.png"]]) {
        const url = new URL(installed.installed.find((pack) => pack.id === latest)[key], f.origin);
        url.searchParams.set("token", token);
        const asset = await fetch(url);
        assert.equal(asset.status, 200);
        assert.deepEqual(Buffer.from(await asset.arrayBuffer()), await readFile(join(candidate, latest, filename)));
    }
    await page.waitForFunction(() => document.querySelector(".pack-stage img")?.src.includes("template@0.2.0"));
    await playing(page, "idle.webm");
    const stateCases = [
        [{ state: "ready", message: "Ready to help whenever you need me. Try asking about your timers, the weather, or a quick reminder." }, "idle"],
        [{ state: "connected" }, "idle"],
        [{ state: "listening" }, "listening"],
        [{ state: "thinking" }, "thinking"],
        [{ state: "speaking" }, "speaking"],
        [{ state: "muted" }, "muted"],
        [{ state: "connecting" }, "connecting"],
        [{ state: "link_code", user_code: "FAKE-CODE", verification_uri: "https://127.0.0.1/link", expires_at_unix: 1 }, "setup"],
        [{ state: "approval", id: "synthetic-approval", prompt: "Allow this action to continue?", accept_label: "Allow", deny_label: "Deny" }, "approval"],
        [{ state: "error", message: "Could not finish the request. Please try again.", relink: true }, "error"],
    ];
    const overlaps = [];
    for (const [state, clip] of stateCases) {
        await f.update(state);
        const file = await playing(page, clip + ".webm");
        const layout = await page.evaluate(() => {
            const boxes = {};
            const selectors = [".pack-stage", ".screen-pack h1", ".pack-clock", ".pack-date", ".screen-pack .details p", ".screen-pack .controls button"];
            for (const selector of selectors) {
                const nodes = [...document.querySelectorAll(selector)].filter((node) => !node.hidden && node.getClientRects().length);
                for (const [i, node] of nodes.entries()) {
                    let bounds = node.getBoundingClientRect();
                    if (node.tagName === "P" && node.closest(".details")) {
                        const range = document.createRange();
                        range.selectNodeContents(node);
                        bounds = range.getBoundingClientRect();
                    }
                    boxes[selector + i] = [bounds.left, bounds.top, bounds.right, bounds.bottom];
                }
            }
            return { boxes, stage: getComputedStyle(document.querySelector(".pack-stage")).backgroundColor, page: getComputedStyle(document.documentElement).backgroundColor };
        });
        assert.equal(layout.stage, layout.page);
        console.log("FACE 960x600", state.state, file, JSON.stringify(layout));
        const boxes = Object.entries(layout.boxes);
        for (const [i, [name, box]] of boxes.entries()) {
            for (const [other, candidate] of boxes.slice(i + 1)) {
                if (Math.min(box[2], candidate[2]) - Math.max(box[0], candidate[0]) > 0 &&
                    Math.min(box[3], candidate[3]) - Math.max(box[1], candidate[1]) > 0) overlaps.push(`${state.state}: ${name} overlaps ${other}`);
            }
        }
    }
    await mkdir("test-results/screenshots", { recursive: true });
    await page.screenshot({ path: "test-results/screenshots/template-face-960x600.png" });
    await f.update({ state: "approval", id: "synthetic-keyboard", prompt: "Use the native keys?", accept_label: "Allow", deny_label: "Deny" });
    await page.getByRole("button", { name: "Allow", exact: true }).focus();
    await page.keyboard.press("Tab");
    await page.keyboard.press("Enter");
    assert.deepEqual((await f.next()).action, { action: "deny", id: "synthetic-keyboard" });
    await f.update({ state: "connected" });
    await playing(page, "idle.webm");
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.waitForFunction(() => !document.querySelector("video:not([hidden])"));
    assert.equal(await page.locator(".pack-stage img").evaluate((node) => node.naturalWidth), 640);
    await page.emulateMedia({ reducedMotion: "no-preference" });
    const mp4 = await f.context.newPage();
    await mp4.addInitScript(() => { HTMLMediaElement.prototype.canPlayType = () => ""; });
    await mp4.goto(page.url());
    await playing(mp4, "idle.mp4");
    await mp4.close();
    const settings = page.getByRole("button", { name: "Settings", exact: true });
    for (const chosen of [id, ""]) {
        await settings.press("Enter");
        await refresh(page);
        await page.getByLabel("Screen pack", { exact: true }).selectOption(chosen);
        await page.getByRole("button", { name: "Use screen pack", exact: true }).click();
        await page.waitForFunction(async ({ chosen, token }) => {
            const result = await fetch("/screen-packs/v1?token=" + token);
            return result.ok && (await result.json()).active === (chosen || null);
        }, { chosen, token });
        await page.getByRole("button", { name: "Back", exact: true }).press("Enter");
        if (chosen) {
            await page.waitForFunction((id) => document.querySelector(".pack-stage img")?.src.includes(id), id);
            await playing(page, "idle.webm");
        } else {
            await page.locator(".pack-stage").waitFor({ state: "detached" });
        }
    }
    const rollback = await catalog();
    assert.deepEqual(rollback.installed.map((pack) => pack.id).sort(), [id, latest]);
    assert.equal((await f.api("/settings/v1")).restart_pending, false);
    console.log("FACE-UPGRADE both_versions=PASS auto_template_0.2.0=PASS media_bytes=PASS rollback=PASS voice_restart=false");
    assert.deepEqual(overlaps, []);
});


test("FACE published fake-adb tree serves identical 0.2.0 assets", { skip: !process.env.FACE_CANDIDATE }, async (t) => {
    const candidate = process.env.FACE_CANDIDATE;
    const latest = "template@0.2.0";
    const f = await open(t, {
        prepare: (home) => cp(join(candidate, "pushed-home", "screens"), join(home, "screens"), { recursive: true }),
        initialState: { state: "connected" },
    });
    const catalog = await f.api("/screen-packs/v1");
    assert.equal(catalog.active, latest);
    assert.deepEqual(catalog.installed.map((pack) => pack.id).sort(), [id, latest]);
    for (const file of ["pack.json", "rest.png", "idle.webm", "muted.mp4"]) {
        const res = await fetch(`${f.origin}/screen-packs/v1/${latest}/${file}?token=${token}`);
        assert.equal(res.status, 200);
        assert.deepEqual(Buffer.from(await res.arrayBuffer()), await readFile(join(candidate, latest, file)));
    }
    await playing(f.page, "idle.webm");
    console.log("FACE-ADB production_catalog=PASS both_versions=PASS media_bytes=PASS");
});