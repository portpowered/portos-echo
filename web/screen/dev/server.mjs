import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { bind, createHandler } from "./handler.mjs";

const assets = {};
for (const [route, name, type] of [
    ["/", "index.html", "text/html"],
    ["/screen.js", "screen.js", "text/javascript"],
    ["/screen.css", "screen.css", "text/css"],
]) {
    assets[route] = {
        type,
        body: await readFile(new URL(`../dist/${name}`, import.meta.url)),
    };
}
const handler = createHandler(assets);
const server = createServer((request, response) => {
    void handler.handle(request, response).catch(() => {
        response.writeHead(500);
        response.end();
    });
});
const shutdown = bind(server, handler, (message, code) => {
    console.log(message);
    if (code) process.exitCode = code;
});
process.once("SIGINT", shutdown);
process.once("SIGTERM", shutdown);
