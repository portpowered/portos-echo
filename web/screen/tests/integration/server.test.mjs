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
    `../../../../target/debug/examples/screen_fixture${process.platform === "win32" ? ".exe" : ""}`,
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
        await page
            .getByRole("heading", { name: state.state, exact: true })
            .waitFor();
    };
    await update({ state: "ready", message: "Echo Live ready" });
    assert.equal(
        await page.locator("section p").textContent(),
        "Echo Live ready",
    );
    await page.getByRole("button", { name: "Start", exact: true }).click();
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
    await page
        .getByRole("status")
        .filter({ hasText: "Action sent; waiting for screen update" })
        .waitFor();
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
    for (const [path, file] of [
        ["/", "index.html"],
        ["/screen.css", "screen.css"],
        ["/screen.js", "screen.js"],
    ]) {
        const response = await fetch(origin + path);
        assert.equal(response.status, 200);
        assert.equal(
            await response.text(),
            await readFile(
                new URL(`../../dist/${file}`, import.meta.url),
                "utf8",
            ),
        );
        assert.equal(response.headers.get("referrer-policy"), "no-referrer");
    }
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
    assert.deepEqual(output, []);
    console.log(
        "native page: exact Ready/Error/Approval; pointer Tap; Enter Approve; Space Deny; focus/1280x800 controls; revoked POST refused once with no action",
    );
});
