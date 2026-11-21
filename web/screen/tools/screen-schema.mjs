import { readFile, writeFile } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";
import prettier from "prettier";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";

const root = new URL("../../../schemas/screen/", import.meta.url);
const sources = {};
for (const file of ["ui_state", "ui_action", "screen_frame", "mouth_batch"]) {
    sources[file] = JSON.parse(
        await readFile(new URL(`${file}.json`, root), "utf8"),
    );
}
function shape(schema) {
    if (schema.$ref) return { ref: schema.$ref.split("/").at(-1) };
    if (schema.oneOf) return { variants: schema.oneOf.map(shape) };
    if (schema.const !== undefined) return { literal: schema.const };
    if (schema.enum)
        return { variants: schema.enum.map((literal) => ({ literal })) };
    if (schema.type === "object") {
        const optional = Object.keys(schema.properties).filter(
            (key) => !(schema.required ?? []).includes(key),
        );
        return {
            fields: Object.fromEntries(
                [...(schema.required ?? []), ...optional].map((key) => [
                    key,
                    shape(schema.properties[key]),
                ]),
            ),
            ...(optional.length ? { optional } : {}),
        };
    }
    if (schema.type === "array") return { array: shape(schema.items) };
    if (schema.format === "uint64") return { type: "u64" };
    return { type: schema.format === "uint8" ? "u8" : schema.type };
}
function type(schema) {
    if (schema.ref) return schema.ref;
    if (schema.variants) return schema.variants.map(type).join(" | ");
    if ("literal" in schema) return JSON.stringify(schema.literal);
    if (schema.fields)
        return `{${Object.entries(schema.fields)
            .map(
                ([key, value]) =>
                    `${key}${schema.optional?.includes(key) ? "?" : ""}: ${type(value)}`,
            )
            .join(";")}}`;
    if (schema.array) return `Array<${type(schema.array)}>`;
    if (schema.type === "u64") return "bigint";
    return ["integer", "u8"].includes(schema.type) ? "number" : schema.type;
}
const models = Object.fromEntries(
    Object.values(sources).map((s) => [s.title, shape(s)]),
);
// The lip-sync batch is the one schema whose definitions have no file of
// their own.
for (const [key, value] of Object.entries(sources.mouth_batch.$defs))
    models[key] = shape(value);
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

// Settings uses ordinary JSON numbers and lossless decimal cap text. Keep its
// HTTP types separate from the legacy SSE runtime model/integer decoder.
const settingsSources = {};
const settingsModels = {};
for (const name of [
    "settings_snapshot_v1",
    "settings_patch_v1",
    "prompt_save_v1",
    "save_result_v1",
    "settings_error_v1",
    "settings_snapshot_v2",
    "settings_patch_v2",
    "save_result_v2",
    "settings_error_v2",
    "portos_operation_v2",
    "portos_operation_started_v2",
    "openai_account_v2",
    "muse_probe_request_v2",
    "muse_probe_result_v2",
]) {
    const schema = JSON.parse(
        await readFile(new URL(`${name}.json`, root), "utf8"),
    );
    settingsSources[name] = schema;
    for (const [key, value] of Object.entries({
        ...schema.$defs,
        [schema.title]: schema,
    })) {
        const model = shape(value);
        if (
            settingsModels[key] &&
            !isDeepStrictEqual(settingsModels[key], model)
        )
            throw new Error(`Conflicting settings definition: ${key}`);
        settingsModels[key] = model;
    }
}
const settingsHash = createHash("sha256")
    .update(JSON.stringify(settingsSources))
    .digest("hex");
const settingsOutput = await prettier.format(
    `// Generated from schemas/screen/*_v{1,2}.json by tools/screen-schema.mjs.\n// Schema SHA-256: ${settingsHash}\n${Object.entries(
        settingsModels,
    )
        .map(([key, value]) => `export type ${key} = ${type(value)};`)
        .join("\n")}\n`,
    {
        ...(await prettier.resolveConfig(fileURLToPath(target), {
            editorconfig: true,
        })),
        parser: "typescript",
    },
);
const settingsTarget = new URL("../src/settings/model.ts", import.meta.url);
if (process.argv.includes("--write"))
    await writeFile(settingsTarget, settingsOutput);
else if (
    (await readFile(settingsTarget, "utf8").catch(() => "")) !== settingsOutput
)
    throw new Error("settings schema drift: regenerate src/settings/model.ts");
