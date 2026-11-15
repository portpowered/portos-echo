import { spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";

await mkdir("test-results", { recursive: true });
const result = spawnSync(
    process.execPath,
    [
        "--test",
        "--test-timeout=30000",
        "--test-concurrency=2",
        "tests/*.test.mjs",
    ],
    { encoding: "utf8" },
);
const transcript = `${result.stdout ?? ""}${result.stderr ?? ""}`;
process.stdout.write(transcript);
await writeFile("test-results/summary.txt", transcript);
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
await import("./coverage.mjs");
