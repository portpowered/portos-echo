// Contact sheets and reference comparison. A sheet tiles every state's render
// at half size so one committed reference image covers a whole set.
import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { browser, writeOut } from "./harness.mjs";

const COLUMNS = 4;
// A pixel differs when any channel moves by more than DELTA; a sheet fails
// when more than SHARE of its pixels differ.
const DELTA = 24;
const SHARE = 0.002;

async function canvasPage() {
    const page = await browser.newPage();
    await page.route("**/*", (route) => route.abort());
    return page;
}

export async function sheet(renders) {
    const page = await canvasPage();
    try {
        const data = await page.evaluate(
            async ({ images, columns }) => {
                const bitmaps = await Promise.all(
                    images.map((b64) =>
                        fetch(`data:image/png;base64,${b64}`)
                            .then((r) => r.blob())
                            .then((blob) => createImageBitmap(blob)),
                    ),
                );
                const w = bitmaps[0].width / 2;
                const h = bitmaps[0].height / 2;
                const rows = Math.ceil(bitmaps.length / columns);
                const canvas = new OffscreenCanvas(w * columns, h * rows);
                const context = canvas.getContext("2d");
                context.fillStyle = "#000";
                context.fillRect(0, 0, canvas.width, canvas.height);
                bitmaps.forEach((bitmap, i) =>
                    context.drawImage(
                        bitmap,
                        (i % columns) * w,
                        Math.floor(i / columns) * h,
                        w,
                        h,
                    ),
                );
                const blob = await canvas.convertToBlob({ type: "image/png" });
                const bytes = new Uint8Array(await blob.arrayBuffer());
                let text = "";
                for (const byte of bytes) text += String.fromCharCode(byte);
                return btoa(text);
            },
            {
                images: renders.map((png) => png.toString("base64")),
                columns: COLUMNS,
            },
        );
        return Buffer.from(data, "base64");
    } finally {
        await page.close();
    }
}

async function difference(current, reference) {
    const page = await canvasPage();
    try {
        return await page.evaluate(
            async ({ current, reference, delta }) => {
                const load = (b64) =>
                    fetch(`data:image/png;base64,${b64}`)
                        .then((r) => r.blob())
                        .then((blob) => createImageBitmap(blob));
                const [a, b] = await Promise.all([
                    load(current),
                    load(reference),
                ]);
                if (a.width !== b.width || a.height !== b.height)
                    return { size: [a.width, a.height, b.width, b.height] };
                const pixels = (bitmap) => {
                    const canvas = new OffscreenCanvas(
                        bitmap.width,
                        bitmap.height,
                    );
                    const context = canvas.getContext("2d");
                    context.drawImage(bitmap, 0, 0);
                    return [
                        canvas,
                        context,
                        context.getImageData(0, 0, bitmap.width, bitmap.height),
                    ];
                };
                const [canvas, context, left] = pixels(a);
                const right = pixels(b)[2];
                let count = 0;
                for (let i = 0; i < left.data.length; i += 4) {
                    let far = false;
                    for (let c = 0; c < 3; c++)
                        far ||=
                            Math.abs(left.data[i + c] - right.data[i + c]) >
                            delta;
                    const grey = left.data[i] / 3;
                    left.data.set(
                        far ? [255, 0, 64, 255] : [grey, grey, grey, 255],
                        i,
                    );
                    if (far) count++;
                }
                context.putImageData(left, 0, 0);
                const blob = await canvas.convertToBlob({ type: "image/png" });
                const bytes = new Uint8Array(await blob.arrayBuffer());
                let text = "";
                for (const byte of bytes) text += String.fromCharCode(byte);
                return { count, total: a.width * a.height, diff: btoa(text) };
            },
            {
                current: current.toString("base64"),
                reference: reference.toString("base64"),
                delta: DELTA,
            },
        );
    } finally {
        await page.close();
    }
}

// Compare a sheet with its committed reference; UPDATE_RENDER_SNAPSHOTS=1
// rewrites the reference instead.
export async function matchReference(name, current) {
    await writeOut(`${name}-sheet.png`, current);
    const path = new URL(`references/${name}.png`, import.meta.url);
    if (process.env.UPDATE_RENDER_SNAPSHOTS === "1") {
        await mkdir(new URL("references/", import.meta.url), {
            recursive: true,
        });
        await writeFile(path, current);
        return;
    }
    const reference = await readFile(path).catch(() => undefined);
    assert.ok(
        reference,
        `render reference ${name}.png is missing; run with UPDATE_RENDER_SNAPSHOTS=1`,
    );
    const result = await difference(current, reference);
    assert.equal(result.size, undefined, `${name} sheet size changed`);
    if (result.count > result.total * SHARE)
        await writeOut(`${name}-diff.png`, Buffer.from(result.diff, "base64"));
    assert.ok(
        result.count <= result.total * SHARE,
        `${name}: ${result.count} of ${result.total} pixels differ from the reference; see ${name}-diff.png`,
    );
}
