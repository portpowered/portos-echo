import { test } from "node:test";
import assert from "node:assert/strict";
import { endpoints, postAction } from "../dist/lib/transport.js";
import { createHandler, bind } from "../dev/handler.mjs";
import { Readable } from "node:stream";

test("same-origin grammar rejects incomplete, credentialed and unsafe URLs", () => {
    assert.deepEqual(
        endpoints("http://127.0.0.1:4173/?events=%2Fevents&actions=%2Factions"),
        {
            events: "http://127.0.0.1:4173/events",
            actions: "http://127.0.0.1:4173/actions",
        },
    );
    for (const href of [
        "http://127.0.0.1/",
        "http://127.0.0.1/?events=/e",
        "http://127.0.0.1/?events=https://example.invalid&actions=/a",
        "http://127.0.0.1/?events=data:x&actions=/a",
        "http://127.0.0.1/?events=http://user@127.0.0.1/e&actions=/a",
    ])
        assert.throws(() => endpoints(href));
});
test("POST means exact JSON and only 204 means accepted; no retries", async (t) => {
    let calls = 0;
    t.mock.method(globalThis, "fetch", async (url, options) => {
        calls++;
        assert.equal(url, "/actions");
        assert.equal(options.method, "POST");
        assert.deepEqual(options.headers, {
            "Content-Type": "application/json",
        });
        assert.equal(options.body, '{"action":"dismiss"}');
        return { status: calls === 1 ? 204 : 403 };
    });
    await postAction(
        "/actions",
        { action: "dismiss" },
        new AbortController().signal,
    );
    await assert.rejects(
        postAction(
            "/actions",
            { action: "dismiss" },
            new AbortController().signal,
        ),
    );
    assert.equal(calls, 2);
});
test("dev handler bounds bodies/queue, emits scripted stream and cleans resources in memory", async () => {
    let tick;
    let cleared = 0;
    const handler = createHandler(
        { "/": { type: "text/html", body: "page" } },
        {
            setInterval: (fn) => {
                tick = fn;
                return 7;
            },
            clearInterval: (id) => {
                assert.equal(id, 7);
                cleared++;
            },
        },
    );
    async function request(method, url, body = "") {
        const req = Readable.from([body]);
        Object.assign(req, { method, url });
        const response = {
            status: 0,
            body: "",
            writeHead(n) {
                this.status = n;
            },
            write(s) {
                this.body += s;
            },
            end(s = "") {
                this.body += s;
            },
        };
        await handler.handle(req, response);
        return response;
    }
    assert.equal((await request("GET", "/")).body, "page");
    assert.equal(
        (await request("GET", "/?events=%2Fevents&actions=%2Factions")).body,
        "page",
    );
    assert.equal((await request("GET", "/../package.json")).status, 404);
    assert.equal((await request("POST", "/actions", "x")).status, 400);
    assert.equal(
        (await request("POST", "/actions", "x".repeat(65537))).status,
        413,
    );
    for (let i = 0; i < 32; i++)
        assert.equal(
            (await request("POST", "/actions", '{"action":"dismiss"}')).status,
            204,
        );
    assert.equal(
        (await request("POST", "/actions", '{"action":"dismiss"}')).status,
        503,
    );
    assert.equal(handler.actions.length, 32);
    const stream = await request("GET", "/events");
    tick();
    assert.equal(
        stream.body,
        'data: {"seq":1,"state":{"state":"ready","message":"Echo Live ready"}}\n\ndata: {"seq":2,"state":{"state":"connecting"}}\n\n',
    );
    handler.close();
    assert.equal(cleared, 1);
    let error;
    let closed = 0;
    const reports = [];
    const shutdown = bind(
        {
            on: (_, fn) => {
                error = fn;
            },
            listen: (options, ready) => {
                assert.deepEqual(options, { host: "127.0.0.1", port: 4173 });
                ready();
            },
            close: () => closed++,
        },
        { close: () => closed++ },
        (...args) => reports.push(args),
    );
    error();
    shutdown();
    assert.equal(closed, 2);
    assert.equal(reports[1][0], "screen-dev: port unavailable");
});
