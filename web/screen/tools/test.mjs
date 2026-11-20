import { spawnSync } from "node:child_process";
import { mkdir, writeFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

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
        "-p",
        "echo-bridge",
        "--example",
        "screen_pack_fixture",
    ],
    { cwd: new URL("../../../", import.meta.url), stdio: "inherit" },
);
if (fixture.error) throw fixture.error;
if (fixture.status !== 0) process.exit(fixture.status ?? 1);
// Fixture preparation belongs to the suite owner. Integration tests consume
// prebuilt executables and generated data; they never compile a binary.
const packRoot = await mkdtemp(join(tmpdir(), "screen-pack-test-"));
for (const version of ["0.1.0", "0.2.0"]) {
    const generated = spawnSync(
        "sh",
        [
            "scripts/make-template-screenpack.sh",
            "--version",
            version,
            "--out",
            join(packRoot, version),
        ],
        { cwd: new URL("../../../", import.meta.url), stdio: "inherit" },
    );
    if (generated.error || generated.status !== 0) {
        await rm(packRoot, { recursive: true, force: true });
        if (generated.error) throw generated.error;
        process.exit(generated.status ?? 1);
    }
}
const result = spawnSync(
    process.execPath,
    [
        "--test",
        "--test-timeout=30000",
        "--test-concurrency=2",
        "tests/*.test.mjs",
        "tests/integration/*.test.mjs",
    ],
    {
        encoding: "utf8",
        env: {
            ...process.env,
            SCREEN_PACK_FIXTURE: join(packRoot, "0.1.0"),
            SCREEN_PACK_EYES_FIXTURE: join(packRoot, "0.2.0"),
        },
    },
);
await rm(packRoot, { recursive: true, force: true });
const transcript = `${result.stdout ?? ""}${result.stderr ?? ""}`;
process.stdout.write(transcript);
await writeFile("test-results/summary.txt", transcript);
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
await import("./coverage.mjs");
