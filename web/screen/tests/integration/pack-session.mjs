import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { cp, mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { archivePeer } from "./pack-peer.mjs";

export const token = "synthetic-pack-pair";
export const activeVideo =
    ".pack-stage video:not([hidden]):not([data-leaving])";
const packStyles = 'link[rel="stylesheet"][href*="/screen-packs/v1/"]';

// Use the prebuilt production bridge/settings fixture and the existing TLS peer.
// Each journey owns its local home, listener, browser context and shutdown.
export async function open(
    t,
    { archivePath, template, viewport, initialState },
) {
    const root = await mkdtemp(join(tmpdir(), "companion-journey-"));
    const home = join(root, "home");
    let peer, child, browser, next, ready, exited;
    t.after(async () => {
        try {
            await browser?.close();
            if (ready) {
                child.stdin.write('{"stop":true}\n');
                assert.deepEqual(await next(), {
                    stopped: true,
                    restart_requested: false,
                });
                assert.deepEqual(await exited, [0, null]);
            }
        } finally {
            child?.kill();
            await peer?.close();
            await rm(root, { recursive: true, force: true });
        }
    });
    await mkdir(join(home, "screens"), { recursive: true });
    await cp(
        join(process.env.SCREEN_PACK_FIXTURE, template),
        join(home, "screens", template),
        { recursive: true },
    );
    const archive = await readFile(archivePath);
    const sha256 = createHash("sha256").update(archive).digest("hex");
    peer = await archivePeer(root, archive, basename(archivePath));
    const executable = fileURLToPath(
        new URL(
            "../../../../target/debug/examples/screen_pack_fixture",
            import.meta.url,
        ),
    );
    child = spawn(executable, [], { stdio: ["pipe", "pipe", "inherit"] });
    exited = once(child, "exit");
    const lines = createInterface({ input: child.stdout })[
        Symbol.asyncIterator
    ]();
    next = async () => JSON.parse((await lines.next()).value);
    const command = (value) => child.stdin.write(JSON.stringify(value) + "\n");
    command({ home, ca: join(root, "ca.pem") });
    ({ ready } = await next());
    assert.ok(Number.isInteger(ready));
    async function send(state) {
        command({ state });
        assert.deepEqual(await next(), { sent: true });
    }
    await send(initialState);
    const origin = `http://127.0.0.1:${ready}`;
    browser = await chromium.launch({
        headless: true,
        args: ["--disable-background-networking", "--no-proxy-server"],
    });
    const context = await browser.newContext({
        viewport,
        locale: "en-US",
        timezoneId: "UTC",
        serviceWorkers: "block",
    });
    const outside = [];
    await context.route("**/*", (route) => {
        if (new URL(route.request().url()).origin === origin)
            return route.continue();
        outside.push(new URL(route.request().url()).origin);
        return route.abort();
    });
    await context.addInitScript(() => {
        window.packRevision = 0;
        // A pack's stylesheet arrives after its layout is mounted, and the
        // page repositions its slots in that link's load handler. Capture runs
        // before the handler in the same dispatch, so a counted link means
        // the slots are already in their styled place.
        window.packStyles = new WeakMap();
        document.addEventListener(
            "load",
            ({ target }) => {
                if (target instanceof HTMLLinkElement)
                    window.packStyles.set(
                        target,
                        (window.packStyles.get(target) ?? 0) + 1,
                    );
            },
            true,
        );
        const NativeEventSource = window.EventSource;
        window.EventSource = class extends NativeEventSource {
            constructor(...args) {
                super(...args);
                this.addEventListener("screen-pack-reload", (event) => {
                    window.packRevision = JSON.parse(event.data).revision;
                });
            }
        };
    });
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    await page.clock.setFixedTime(new Date("2026-11-20T15:04:00.000Z"));
    const query = new URLSearchParams({
        events: `/events?token=${token}`,
        actions: `/actions?token=${token}`,
    });
    await page.goto(`${origin}/?${query}`);
    let sequence = 1;
    async function update(state) {
        await send(state);
        await page.locator(`[data-seq="${++sequence}"]`).waitFor();
    }
    async function api(path) {
        const response = await fetch(`${origin}${path}?token=${token}`);
        assert.equal(response.status, 200);
        return response.json();
    }
    return { page, context, home, peer, sha256, update, api, next, outside };
}

// The named clip of the named pack is decoding and the pack's own stylesheet
// has been applied, so slot geometry read after this is the styled layout.
export async function playing(page, name, identity) {
    const loads = await page.waitForFunction(
        ({ selector, name, identity, styles }) => {
            const video = document.querySelector(selector);
            const loads = [...document.querySelectorAll(styles)].map(
                (link) => window.packStyles.get(link) ?? 0,
            );
            return (
                video &&
                new URL(video.src).pathname.endsWith(`/${identity}/${name}`) &&
                video.readyState >= 2 &&
                video.getVideoPlaybackQuality().totalVideoFrames > 0 &&
                loads.every((count) => count > 0) &&
                loads
            );
        },
        { selector: activeVideo, name, identity, styles: packStyles },
    );
    // A second load means the page detached the link, which leaves the pack
    // unstyled until the sheet is fetched again.
    assert.ok(
        (await loads.jsonValue()).every((count) => count === 1),
        "a mounted pack loads its stylesheet once",
    );
    return page
        .locator(activeVideo)
        .evaluate((node) => new URL(node.src).pathname.split("/").at(-1));
}

export async function refresh(page) {
    await page
        .getByRole("button", { name: /^(Load screen packs|Refresh packs)$/ })
        .click();
    await page.getByText("Screen packs loaded", { exact: true }).waitFor();
}
