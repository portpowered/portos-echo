import { test, after } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFile } from "node:fs/promises";
import { chromium } from "playwright";

// T3: the built page in a real browser against a loopback stand-in for the
// bridge. `bridge.document` is the build of the page it serves, `bridge.named`
// the build its document names to a running page that asks; they differ only
// for a page that cannot get the new document.
const A = "a".repeat(16);
const B = "b".repeat(16);
const C = "c".repeat(16);
const asset = (name) =>
    readFile(new URL(`../../dist/${name}`, import.meta.url));
const index = await asset("index.html").then(String);
const assets = {
    "/screen.js": ["text/javascript", await asset("screen.js")],
    "/screen.css": ["text/css", await asset("screen.css")],
    "/tokens.css": ["text/css", await asset("tokens.css")],
};
const html = (build) =>
    build
        ? index.replace(
              "<head>",
              `<head>\n    <meta name="echo-build" content="${build}" />`,
          )
        : index;
const bridge = {
    document: A,
    named: A,
    documents: 0,
    checks: 0,
    seq: 0,
    state: "listening",
    // Events requests answered 503, as a bridge still stopping answers.
    refuse: 0,
    refused: 0,
};
const streams = new Set();
const server = createServer((request, response) => {
    const path = request.url.split("?")[0];
    const headers = { "Cache-Control": "no-store" };
    if (path === "/events" && bridge.refuse > 0) {
        bridge.refuse--;
        bridge.refused++;
        response.writeHead(503, headers);
        response.end();
    } else if (path === "/events") {
        response.writeHead(200, {
            ...headers,
            "Content-Type": "text/event-stream",
        });
        response.write(
            `retry: 20\n\ndata: ${JSON.stringify({ seq: ++bridge.seq, state: { state: bridge.state } })}\n\n`,
        );
        streams.add(response);
        response.on("close", () => streams.delete(response));
    } else if (path === "/") {
        // A navigation gets the page; the running page's own fetch asks.
        const navigation = request.headers["sec-fetch-dest"] === "document";
        if (navigation) bridge.documents++;
        else bridge.checks++;
        response.writeHead(200, { ...headers, "Content-Type": "text/html" });
        response.end(html(navigation ? bridge.document : bridge.named));
    } else if (assets[path]) {
        response.writeHead(200, {
            ...headers,
            "Content-Type": assets[path][0],
        });
        response.end(assets[path][1]);
    } else {
        response.writeHead(404, headers);
        response.end();
    }
});
server.listen({ host: "127.0.0.1", port: 0 });
await once(server, "listening");
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({
    headless: true,
    args: ["--disable-background-networking", "--no-proxy-server"],
});
after(async () => {
    await browser.close();
    for (const stream of streams) stream.destroy();
    server.close();
});

async function until(count, message) {
    for (let waited = 0; waited < 10000; waited += 10) {
        if (count()) return;
        await new Promise((resolve) => setTimeout(resolve, 10));
    }
    assert.fail(message);
}
// The bridge restarts: every events stream ends and the page connects again.
// The page asks for the document once per connection.
async function restart(expected) {
    for (const stream of streams) stream.destroy();
    await until(
        () => bridge.checks === expected,
        `${bridge.checks} checks, expected ${expected}`,
    );
}

test("upgrade: a page reconnecting to a bridge with another page build reloads once and runs that build", async (t) => {
    const context = await browser.newContext({ serviceWorkers: "block" });
    t.after(() => context.close());
    await context.route("**/*", (route) =>
        new URL(route.request().url()).origin === origin
            ? route.continue()
            : route.abort(),
    );
    const page = await context.newPage();
    const running = () =>
        page.locator('meta[name="echo-build"]').getAttribute("content");
    await page.goto(`${origin}/?events=%2Fevents&actions=%2Factions`);
    await page.locator('[data-ui-state="listening"]').waitFor();
    await until(() => bridge.checks === 1, "the first connection asks");
    // The same bridge build again: no reload.
    await restart(2);
    // A bridge that names no build, as an older one: no reload.
    bridge.named = "";
    await restart(3);
    assert.deepEqual([bridge.documents, await running()], [1, A]);
    // The upgraded bridge: the fourth check reloads the page, and the page
    // it gets asks once on its own first connection.
    bridge.document = bridge.named = B;
    await restart(5);
    await page.locator('[data-ui-state="listening"]').waitFor();
    assert.deepEqual([bridge.documents, await running()], [2, B]);
    await restart(6);
    assert.deepEqual([bridge.documents, await running()], [2, B]);
    // A page that cannot get the build the bridge names reloads for it once,
    // not in a loop: the reloaded page (8) and a later reconnect (9) stay.
    bridge.named = C;
    await restart(8);
    await restart(9);
    await page.locator('[data-ui-state="listening"]').waitFor();
    assert.deepEqual([bridge.documents, await running()], [3, B]);
    assert.equal(
        await page.evaluate(() => sessionStorage.getItem("echo-screen-reload")),
        C,
    );
    // Back to the build the page runs: nothing is pending, nothing reloads.
    bridge.named = B;
    await restart(10);
    assert.deepEqual([bridge.documents, bridge.checks], [3, 10]);
    console.log(
        "upgrade: same and absent build kept the page; build B reloaded it once; an unreachable build reloaded once, no loop",
    );
});

test("restart: a page shows the frames of a bridge that restarted and counts from one again", async (t) => {
    const context = await browser.newContext({ serviceWorkers: "block" });
    t.after(() => context.close());
    await context.route("**/*", (route) =>
        new URL(route.request().url()).origin === origin
            ? route.continue()
            : route.abort(),
    );
    Object.assign(bridge, { document: A, named: A, state: "listening" });
    const page = await context.newPage();
    const shown = () =>
        page
            .locator(".screen-presentation")
            .evaluate((node) => [node.dataset.uiState, node.dataset.seq]);
    // The running bridge has sent many frames.
    bridge.seq = 40;
    await page.goto(`${origin}/?events=%2Fevents&actions=%2Factions`);
    await page.locator('[data-ui-state="listening"]').waitFor();
    assert.deepEqual(await shown(), ["listening", "41"]);
    // A settings save restarts it in place with the same page build: no
    // reload, and the new process numbers its first frame one.
    const { checks, documents } = bridge;
    Object.assign(bridge, { seq: 0, state: "speaking" });
    await restart(checks + 1);
    await page.locator('[data-ui-state="speaking"]').waitFor();
    assert.deepEqual(await shown(), ["speaking", "1"]);
    assert.equal(bridge.documents, documents);
    // On that connection frames are ordered by number as before.
    const [stream] = streams;
    const send = (seq, state) =>
        stream.write(`data: ${JSON.stringify({ seq, state: { state } })}

`);
    send(2, "connected");
    await page.locator('[data-ui-state="connected"]').waitFor();
    send(1, "thinking");
    send(3, "muted");
    await page.locator('[data-ui-state="muted"]').waitFor();
    assert.deepEqual(await shown(), ["muted", "3"]);
    console.log(
        "restart: frame 1 of a restarted bridge replaced frame 41; a stale frame on the same connection stayed out",
    );
});

test("refused: a stream the restarting bridge answered with an error opens again and clears the interrupted line", async (t) => {
    const context = await browser.newContext({ serviceWorkers: "block" });
    t.after(() => context.close());
    await context.route("**/*", (route) =>
        new URL(route.request().url()).origin === origin
            ? route.continue()
            : route.abort(),
    );
    Object.assign(bridge, {
        document: A,
        named: A,
        state: "listening",
        seq: 0,
        refuse: 0,
        refused: 0,
    });
    const page = await context.newPage();
    const status = () => page.getByRole("status").textContent();
    await page.goto(`${origin}/?events=%2Fevents&actions=%2Factions`);
    await page.locator('[data-ui-state="listening"]').waitFor();
    assert.equal(await status(), "");
    // The bridge restarts and refuses the first two attempts to connect:
    // the browser gives up on a refused stream, the page does not.
    Object.assign(bridge, { seq: 0, state: "speaking", refuse: 2 });
    const started = Date.now();
    for (const stream of streams) stream.destroy();
    await page
        .getByRole("status")
        .filter({ hasText: "Screen connection interrupted" })
        .waitFor();
    await page.locator('[data-ui-state="speaking"]').waitFor({ timeout: 8000 });
    const back = Date.now() - started;
    assert.equal(bridge.refused, 2);
    assert.equal(await status(), "");
    // Two refusals are reopened after 1 s and then 2 s: back within 5 s.
    assert.ok(back < 5000, `back after ${back} ms`);
    // An open stream resets the delays, and the page holds one stream.
    bridge.refuse = 1;
    const again = Date.now();
    for (const stream of streams) stream.destroy();
    await until(() => bridge.refused === 3, "the bridge refuses once more");
    await page.locator('[data-ui-state="speaking"]').waitFor();
    await until(() => streams.size === 1, `${streams.size} streams open`);
    assert.equal(await status(), "");
    assert.ok(Date.now() - again < 2900, "the reopen delay started over");
    console.log(
        `refused: two 503 answers, the page was back after ${back} ms and the line cleared; one stream open`,
    );
});
