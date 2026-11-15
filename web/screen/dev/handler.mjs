import { decodeAction } from "../dist/lib/decoder.js";

export const examples = [
    { state: "ready", message: "Echo Live ready" },
    ...[
        "connecting",
        "connected",
        "listening",
        "thinking",
        "speaking",
        "muted",
    ].map((state) => ({ state })),
    {
        state: "link_code",
        user_code: "DEMO-CODE",
        verification_uri: "https://example.invalid/verify",
        expires_at_unix: 1700000000,
    },
    {
        state: "approval",
        id: "approval-1",
        prompt: "Continue?",
        accept_label: "Allow",
        deny_label: "Deny",
    },
    {
        state: "emotion",
        name: "happy",
        frame_ms: 80,
        frames: ["frame-a", "frame-b"],
    },
    { state: "error", message: "Please relink", relink: true },
];
export function createHandler(assets, clock = globalThis) {
    const streams = new Set();
    const actions = [];
    async function handle(request, response) {
        const path = request.url.split("?")[0];
        if (request.method === "GET" && Object.hasOwn(assets, path)) {
            const asset = assets[path];
            response.writeHead(200, { "Content-Type": asset.type });
            response.end(asset.body);
        } else if (request.method === "GET" && request.url === "/events") {
            if (streams.size === 32) {
                response.writeHead(503);
                response.end();
                return;
            }
            response.writeHead(200, {
                "Content-Type": "text/event-stream",
                "Cache-Control": "no-cache",
            });
            let seq = 0;
            const emit = () => {
                if (
                    response.write(
                        `data: ${JSON.stringify({ seq: seq + 1, state: examples[seq++ % examples.length] })}\n\n`,
                    ) === false
                )
                    close();
            };
            const timer = clock.setInterval(emit, 3000);
            const close = () => {
                if (!streams.has(close)) return;
                clock.clearInterval(timer);
                streams.delete(close);
                response.end();
            };
            streams.add(close);
            emit();
            request.on("close", close);
        } else if (request.method === "POST" && request.url === "/actions") {
            let bytes = 0;
            const chunks = [];
            for await (const chunk of request) {
                bytes += Buffer.byteLength(chunk);
                if (bytes > 65536) {
                    response.writeHead(413);
                    response.end();
                    return;
                }
                chunks.push(Buffer.from(chunk));
            }
            try {
                const action = decodeAction(
                    Buffer.concat(chunks).toString("utf8"),
                );
                if (actions.length === 32) {
                    response.writeHead(503);
                    response.end();
                    return;
                }
                actions.push(action);
                response.writeHead(204);
            } catch {
                response.writeHead(400);
            }
            response.end();
        } else {
            response.writeHead(404);
            response.end();
        }
    }
    return {
        handle,
        actions,
        close: () => {
            for (const close of streams) close();
        },
    };
}

export function bind(server, handler, report) {
    server.on("error", () => report("screen-dev: port unavailable", 1));
    server.listen({ host: "127.0.0.1", port: 4173 }, () =>
        report(
            "screen-dev: http://127.0.0.1:4173/?events=%2Fevents&actions=%2Factions",
            0,
        ),
    );
    return () => {
        handler.close();
        server.close();
    };
}
