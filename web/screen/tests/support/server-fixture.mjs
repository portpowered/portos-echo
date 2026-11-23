import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { once } from "node:events";

export async function startScreenFixture(wifi = false) {
    const executable = new URL(
        "../../../../target/debug/examples/screen_fixture",
        import.meta.url,
    );
    const fixture = spawn(fileURLToPath(executable), [], {
        stdio: ["pipe", "pipe", "pipe"],
    });
    const exited = once(fixture, "exit");
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
            : new Promise((resolve, reject) => {
                  waiters.push(resolve);
                  void exited.then(
                      () => reject(new Error("Screen fixture exited")),
                      reject,
                  );
              });
    const command = (value) =>
        fixture.stdin.write(JSON.stringify(value) + "\n");
    const token = "synthetic-screen-pair";
    command({ start: { token, port: 0, wifi } });
    const { ready } = await next();
    assert.equal(ready.host, "127.0.0.1");
    const origin = `http://127.0.0.1:${ready.port}`;
    const pairedUrl = `${origin}/?${new URLSearchParams({
        events: `/events?token=${token}`,
        actions: `/actions?token=${token}`,
    })}`;
    async function stop() {
        command({ stop: true });
        assert.deepEqual(await next(), { stopped: true });
        assert.deepEqual(await exited, [0, null]);
        assert.equal(diagnostic, "");
    }
    return { origin, pairedUrl, command, next, output, stop };
}
