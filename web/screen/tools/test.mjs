import { spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";

await mkdir("test-results", { recursive: true });
const fixture = spawnSync(
    "cargo",
    [
        "build",
        "--offline",
        "--locked",
        "-p",
        "echo-screen-server",
        "--example",
        "screen_fixture",
    ],
    { cwd: new URL("../../../", import.meta.url), stdio: "inherit" },
);
if (fixture.error) throw fixture.error;
if (fixture.status !== 0) process.exit(fixture.status ?? 1);
const result = spawnSync(
    process.execPath,
    [
        "--test",
        "--test-timeout=30000",
        "--test-concurrency=2",
        "tests/*.test.mjs",
        "tests/integration/*.test.mjs",
    ],
    { encoding: "utf8" },
);
const transcript = `${result.stdout ?? ""}${result.stderr ?? ""}`;
process.stdout.write(transcript);
await writeFile("test-results/summary.txt", transcript);
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
await import("./coverage.mjs");
