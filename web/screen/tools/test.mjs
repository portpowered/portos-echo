import { spawn, spawnSync } from "node:child_process";
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
function generate(script, args) {
    return new Promise((resolve) => {
        const child = spawn("sh", [`scripts/${script}`, ...args], {
            cwd: new URL("../../../", import.meta.url),
            stdio: "inherit",
        });
        child.on("error", (error) => resolve({ error }));
        child.on("exit", (status) => resolve({ script, status }));
    });
}

const stagedCompanion = process.env.SCREEN_COMPANION_ARCHIVE;
const companionDir = join(packRoot, "companion");
let result;
try {
    // Independent fixture packs encode in parallel.
    const generations = [
        ["0.1.0", "template"],
        ["0.2.0", "template"],
        ["0.3.0", "default"],
    ].map(([version, name]) =>
        generate("make-template-screenpack.sh", [
            "--version",
            version,
            "--name",
            name,
            "--out",
            join(packRoot, version),
        ]),
    );
    if (stagedCompanion === undefined) {
        generations.push(
            generate("make-companion-screenpack.sh", [
                "--version",
                "0.2.0",
                "--out",
                companionDir,
            ]),
        );
    }
    for (const generated of await Promise.all(generations)) {
        if (generated.error) throw generated.error;
        if (generated.status !== 0) {
            throw new Error(
                `${generated.script} exited with status ${generated.status ?? 1}`,
            );
        }
    }
    result = spawnSync(
        process.execPath,
        [
            "--test",
            "--test-timeout=30000",
            "--test-concurrency=2",
            "tests/*.test.mjs",
            "tests/integration/*.test.mjs",
            "tests/render-snapshots/*.test.mjs",
        ],
        {
            encoding: "utf8",
            env: {
                ...process.env,
                SCREEN_PACK_FIXTURE: join(packRoot, "0.1.0"),
                SCREEN_PACK_EYES_FIXTURE: join(packRoot, "0.2.0"),
                SCREEN_PACK_DEFAULT_FIXTURE: join(packRoot, "0.3.0"),
                SCREEN_COMPANION_ARCHIVE:
                    stagedCompanion ??
                    join(companionDir, "companion-0.2.0.screenpack.tar.gz"),
            },
        },
    );
} finally {
    await rm(packRoot, { recursive: true, force: true });
}
const transcript = `${result.stdout ?? ""}${result.stderr ?? ""}`;
process.stdout.write(transcript);
await writeFile("test-results/summary.txt", transcript);
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
await import("./coverage.mjs");
