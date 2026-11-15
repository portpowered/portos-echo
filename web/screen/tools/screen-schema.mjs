import { readFile, writeFile } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";
import prettier from "prettier";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";

const root = new URL("../../../schemas/screen/", import.meta.url);
const sources = {};
for (const file of ["ui_state", "ui_action", "screen_frame"]) {
    sources[file] = JSON.parse(
        await readFile(new URL(`${file}.json`, root), "utf8"),
    );
}
function shape(schema) {
    if (schema.$ref) return { ref: schema.$ref.split("/").at(-1) };
    if (schema.oneOf) return { variants: schema.oneOf.map(shape) };
    if (schema.const !== undefined) return { literal: schema.const };
    if (schema.type === "object") {
        return {
            fields: Object.fromEntries(
                schema.required.map((key) => [
                    key,
                    shape(schema.properties[key]),
                ]),
            ),
        };
    }
    if (schema.type === "array") return { array: shape(schema.items) };
    return { type: schema.format === "uint64" ? "u64" : schema.type };
}
function type(schema) {
    if (schema.ref) return schema.ref;
    if (schema.variants) return schema.variants.map(type).join(" | ");
    if ("literal" in schema) return JSON.stringify(schema.literal);
    if (schema.fields)
        return `{${Object.entries(schema.fields)
            .map(([key, value]) => `${key}: ${type(value)}`)
            .join(";")}}`;
    if (schema.array) return `Array<${type(schema.array)}>`;
    return schema.type === "u64" ? "bigint" : schema.type;
}
const models = Object.fromEntries(
    Object.values(sources).map((s) => [s.title, shape(s)]),
);
if (
    !isDeepStrictEqual(
        shape(sources.screen_frame.$defs.UiState),
        models.UiState,
    )
)
    throw new Error("screen_frame.$defs.UiState differs from ui_state schema");
const fingerprint = createHash("sha256")
    .update(JSON.stringify(sources))
    .digest("hex");
const output = await prettier.format(
    `// Generated from schemas/screen/*.json by tools/screen-schema.mjs.\n// Schema SHA-256: ${fingerprint}\n${Object.entries(
        models,
    )
        .map(([key, value]) => `export type ${key} = ${type(value)};`)
        .join(
            "\n",
        )}\nexport const models = ${JSON.stringify(models)} as const;\n`,
    {
        ...(await prettier.resolveConfig(
            fileURLToPath(new URL("../src/model.ts", import.meta.url)),
            { editorconfig: true },
        )),
        parser: "typescript",
    },
);
const target = new URL("../src/model.ts", import.meta.url);
if (process.argv.includes("--write")) await writeFile(target, output);
else {
    const actual = await readFile(target, "utf8").catch(() => "");
    if (!isDeepStrictEqual(actual, output)) {
        const line = output
            .split("\n")
            .findIndex((value, index) => value !== actual.split("\n")[index]);
        throw new Error(
            `screen schema drift at model.ts:${line + 1}: expected ${output.split("\n")[line]}`,
        );
    }
    console.log("screen schema: current");
}
