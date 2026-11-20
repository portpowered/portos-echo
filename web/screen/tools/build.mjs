import { mkdir, copyFile, rm } from "node:fs/promises";
import { build } from "esbuild";

await mkdir("dist", { recursive: true });
await rm(new URL("../test-results/coverage", import.meta.url), {
    recursive: true,
    force: true,
});
await build({
    entryPoints: ["src/bootstrap.ts"],
    bundle: true,
    format: "iife",
    target: "es2020",
    sourcemap: true,
    outfile: "dist/screen.js",
});
await build({
    entryPoints: [
        "src/decoder.ts",
        "src/state.ts",
        "src/transport.ts",
        "src/emotion.ts",
        "src/packs/player.ts",
    ],
    outdir: "dist/lib",
    platform: "neutral",
    format: "esm",
    bundle: true,
});
for (const file of ["index.html", "screen.css"])
    await copyFile(`src/${file}`, `dist/${file}`);
