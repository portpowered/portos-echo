import { readFile, readdir, appendFile } from "node:fs/promises";
import v8ToIstanbul from "v8-to-istanbul";
import coverage from "istanbul-lib-coverage";

const map = coverage.createCoverageMap({});
for (const file of await readdir("test-results/coverage")) {
    for (const entry of JSON.parse(
        await readFile(`test-results/coverage/${file}`, "utf8"),
    )) {
        const converter = v8ToIstanbul("dist/screen.js", 0);
        await converter.load();
        converter.applyCoverage(entry.functions);
        map.merge(converter.toIstanbul());
    }
}
const lines = map.getCoverageSummary().lines;
const summary = `Screen TypeScript lines: ${lines.covered}/${lines.total} (${lines.pct}%), required >=85%\n`;
console.log(summary);
await appendFile("test-results/summary.txt", summary);
if (lines.total === 0 || lines.pct < 85)
    throw new Error("Screen changed-line coverage below 85%");
