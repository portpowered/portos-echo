import {
    models,
    type MouthBatch,
    type ScreenFrame,
    type UiAction,
    type UiState,
} from "./model";

// Tokenize outside strings before JSON.parse can round an integer token.
export function parseLossless(wire: string): unknown {
    let cursor = 0;
    function value(): unknown {
        while (/[ \t\r\n]/.test(wire[cursor] ?? "x")) cursor++;
        const tail = wire.slice(cursor);
        const token =
            /^("(?:[^"\\\u0000-\u001f]|\\(?:["\\/bfnrt]|u[0-9a-fA-F]{4}))*"|true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(
                tail,
            )?.[0];
        if (token) {
            cursor += token.length;
            return /^-?\d+$/.test(token) && token !== "-0"
                ? BigInt(token)
                : JSON.parse(token);
        }
        const start = wire[cursor++];
        if (start !== "{" && start !== "[") throw new Error("Invalid JSON");
        const result: Record<string, unknown> = Object.create(null);
        const items: unknown[] = [];
        const close = start === "{" ? "}" : "]";
        while (/[ \t\r\n]/.test(wire[cursor] ?? "x")) cursor++;
        if (wire[cursor] === close) {
            cursor++;
            return start === "{" ? result : items;
        }
        for (;;) {
            if (start === "{") {
                const key = value();
                if (typeof key !== "string") throw new Error("Invalid key");
                while (/[ \t\r\n]/.test(wire[cursor] ?? "x")) cursor++;
                if (wire[cursor++] !== ":") throw new Error("Missing colon");
                result[key] = value();
            } else items.push(value());
            while (/[ \t\r\n]/.test(wire[cursor] ?? "x")) cursor++;
            const end = wire[cursor++];
            if (end === close) return start === "{" ? result : items;
            if (end !== ",") throw new Error("Invalid delimiter");
        }
    }
    const result = value();
    if (!/^[ \t\r\n]*$/.test(wire.slice(cursor)))
        throw new Error("Trailing JSON");
    return result;
}

type Shape = {
    readonly ref?: string;
    readonly variants?: readonly Shape[];
    readonly literal?: string;
    readonly fields?: Readonly<Record<string, Shape>>;
    readonly array?: Shape;
    readonly type?: string;
};
function validate(value: unknown, shape: Shape): unknown {
    if (shape.ref)
        return validate(value, models[shape.ref as keyof typeof models]);
    if (shape.variants) {
        for (const variant of shape.variants) {
            try {
                return validate(value, variant);
            } catch {
                /* Try the next published variant. */
            }
        }
        throw new Error("Unknown message");
    }
    if (shape.fields) {
        if (!value || typeof value !== "object" || Array.isArray(value))
            throw new Error("Expected object");
        return Object.fromEntries(
            Object.entries(shape.fields).map(([key, field]) => [
                key,
                validate((value as Record<string, unknown>)[key], field),
            ]),
        );
    }
    if (shape.array && Array.isArray(value))
        return value.map((item) => validate(item, shape.array as Shape));
    if (shape.literal !== undefined && value === shape.literal) return value;
    if (
        shape.type === "u64" &&
        typeof value === "bigint" &&
        value >= 0n &&
        value <= 18446744073709551615n
    )
        return value;
    if (
        shape.type === "u8" &&
        typeof value === "bigint" &&
        value >= 0n &&
        value <= 255n
    )
        return Number(value);
    if (
        (shape.type === "string" && typeof value === "string") ||
        (shape.type === "boolean" && typeof value === "boolean")
    )
        return value;
    throw new Error("Invalid field");
}
export function decodeFrame(wire: string): ScreenFrame {
    return validate(parseLossless(wire), models.ScreenFrame) as ScreenFrame;
}
export function decodeState(wire: string): UiState {
    return validate(parseLossless(wire), models.UiState) as UiState;
}
export function decodeMouth(wire: string): MouthBatch {
    return validate(parseLossless(wire), models.MouthBatch) as MouthBatch;
}
export function decodeAction(wire: string): UiAction {
    return validate(parseLossless(wire), models.UiAction) as UiAction;
}
export function encode(value: unknown): string {
    if (typeof value === "bigint") return value.toString();
    if (Array.isArray(value)) return `[${value.map(encode).join(",")}]`;
    if (value && typeof value === "object")
        return `{${Object.entries(value)
            .map(([key, item]) => `${JSON.stringify(key)}:${encode(item)}`)
            .join(",")}}`;
    return JSON.stringify(value);
}
