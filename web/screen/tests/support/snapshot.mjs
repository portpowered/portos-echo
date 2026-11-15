import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import assert from "node:assert/strict";

export async function snapshot(page, name) {
    await mkdir("test-results/screenshots", { recursive: true });
    const pixels = await page.screenshot({
        path: `test-results/screenshots/${name}.png`,
    });
    const hash = createHash("sha256").update(pixels).digest("hex");
    const path = new URL(
        `../goldens/${process.platform}.json`,
        import.meta.url,
    );
    const goldens = JSON.parse(await readFile(path, "utf8").catch(() => "{}"));
    if (process.env.UPDATE_SCREEN_SNAPSHOTS === "1") {
        goldens[name] = hash;
        await writeFile(path, JSON.stringify(goldens, null, 2) + "\n");
    }
    assert.equal(
        hash,
        goldens[name],
        `pixel snapshot ${process.platform}/${name}`,
    );
    return hash;
}
