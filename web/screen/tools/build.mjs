import { mkdir, copyFile, readFile, rm, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { tokensCss } from "./tokens.mjs";

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
        "src/packs/mouth.ts",
    ],
    outdir: "dist/lib",
    platform: "neutral",
    format: "esm",
    bundle: true,
});
await mkdir("dist/fonts", { recursive: true });
for (const file of ["index.html", "fonts/manrope-latin-wght-normal.woff2"])
    await copyFile(`src/${file}`, `dist/${file}`);
const styles = await Promise.all(
    ["screen.css", "chrome/screen.css"].map((file) =>
        readFile(`src/${file}`, "utf8"),
    ),
);
await writeFile("dist/screen.css", styles.join("\n"));
await writeFile("dist/tokens.css", await tokensCss());
