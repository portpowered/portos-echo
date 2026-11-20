import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { once } from "node:events";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, stat, utimes } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { archivePeer } from "./pack-peer.mjs";

const token = "synthetic-pack-pair";
const id = "template@0.1.0";
const generated = process.env.SCREEN_PACK_FIXTURE;
assert.ok(generated, "suite owner must prepare generated assets");

async function open(t) {
    const root = await mkdtemp(join(tmpdir(), "screen-pack-journey-"));
    const home = join(root, "home");
    await mkdir(home);
    const archive = await readFile(
        join(generated, "template-0.1.0.screenpack.tar.gz"),
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
    const origin = `http://127.0.0.1:${ready}`;
    const context = await browser.newContext({
        viewport: { width: 1280, height: 800 },
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
    const query = new URLSearchParams({
        events: `/events?token=${token}`,
        actions: `/actions?token=${token}`,
    });
    await page.goto(`${origin}/?${query}`);
    const update = async (state) => {
        command({ state });
        assert.deepEqual(await next(), { sent: true });
        await page
            .getByRole("heading", { name: state.state, exact: true })
            .waitFor();
    };
    const api = async (path) => {
        const response = await fetch(`${origin}${path}?token=${token}`);
        assert.equal(response.status, 200);
        return response.json();
    };
    return { page, context, home, peer, sha256, update, api, outside, origin };
}

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
