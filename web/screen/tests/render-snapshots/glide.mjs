// Idle to speaking from several points of the idle loop: sample the shown
// eye centroid (frame centroid plus the glide's translate) on every
// animation frame and report the largest frame-to-frame jump in CSS px.
const STARTS = [1.9, 5.4, 7.7, 10.5, 12.3];

function sample(page) {
    return page.evaluate(
        () =>
            new Promise((resolve) => {
                const points = [];
                const canvas = new OffscreenCanvas(240, 150);
                const context = canvas.getContext("2d", {
                    willReadFrequently: true,
                });
                const started = performance.now();
                function step() {
                    const video = document.querySelector(
                        ".pack-stage video:not([hidden]):not([data-leaving])",
                    );
                    const box = video.getBoundingClientRect();
                    const scale = box.width / canvas.width;
                    context.clearRect(0, 0, canvas.width, canvas.height);
                    context.drawImage(video, 0, 0, canvas.width, canvas.height);
                    const { data } = context.getImageData(
                        0,
                        0,
                        canvas.width,
                        canvas.height,
                    );
                    let sx = 0;
                    let sy = 0;
                    let n = 0;
                    for (let i = 0; i < data.length; i += 4)
                        if (
                            data[i + 3] >= 128 &&
                            Math.max(data[i], data[i + 1], data[i + 2]) >= 128
                        ) {
                            const p = i / 4;
                            sx += p % canvas.width;
                            sy += Math.floor(p / canvas.width);
                            n++;
                        }
                    const shift = new DOMMatrix(
                        getComputedStyle(video).transform,
                    );
                    if (n)
                        points.push([
                            (sx / n) * scale + shift.e,
                            (sy / n) * scale + shift.f,
                            new URL(video.src).pathname.split("/").at(-1),
                        ]);
                    if (performance.now() - started < 700)
                        requestAnimationFrame(step);
                    else resolve(points);
                }
                requestAnimationFrame(step);
            }),
    );
}

export async function glideRuns(page, emit, hold) {
    const runs = [];
    for (const start of STARTS) {
        await emit(page, { state: "ready", message: "" });
        await page.waitForTimeout(600);
        await hold(page, "idle.webm", Math.round(start * 30));
        await page.evaluate(() =>
            document.querySelector(".pack-stage video:not([hidden])").play(),
        );
        const points = sample(page);
        await emit(page, { state: "speaking" });
        const trace = await points;
        let jump = 0;
        for (let i = 1; i < trace.length; i++)
            jump = Math.max(
                jump,
                Math.hypot(
                    trace[i][0] - trace[i - 1][0],
                    trace[i][1] - trace[i - 1][1],
                ),
            );
        runs.push({
            start,
            jump: Math.round(jump * 10) / 10,
            frames: trace.length,
            ended: trace.at(-1)?.[2],
        });
    }
    return runs;
}
