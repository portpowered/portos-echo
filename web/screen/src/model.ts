// Generated from schemas/screen/*.json by tools/screen-schema.mjs.
// Schema SHA-256: b10b018b2b44ed8ec901f6d14985218a6a3d40405f3ef4548e0556b9a8fc43f7
export type UiState =
    | { state: "ready"; message: string }
    | { state: "connecting" }
    | { state: "connected" }
    | { state: "listening" }
    | { state: "thinking" }
    | { state: "speaking" }
    | { state: "muted" }
    | {
          state: "link_code";
          user_code: string;
          verification_uri: string;
          expires_at_unix: bigint;
      }
    | {
          state: "approval";
          id: string;
          prompt: string;
          accept_label: string;
          deny_label: string;
      }
    | {
          state: "emotion";
          name: string;
          frame_ms: bigint;
          frames: Array<string>;
      }
    | { state: "error"; message: string; relink: boolean };
export type UiAction =
    | { action: "tap"; target: string }
    | { action: "approve"; id: string }
    | { action: "deny"; id: string }
    | { action: "relink" }
    | { action: "dismiss" };
export type ScreenFrame = { seq: bigint; state: UiState };
export type MouthBatch = { sent_ms: bigint; samples: Array<MouthSample> };
export type MouthSample = { at_ms: bigint; open: number; vowel: MouthVowel };
export type MouthVowel = "none" | "a" | "i" | "u" | "e" | "o";
export const models = {
    UiState: {
        variants: [
            {
                fields: {
                    state: { literal: "ready" },
                    message: { type: "string" },
                },
            },
            { fields: { state: { literal: "connecting" } } },
            { fields: { state: { literal: "connected" } } },
            { fields: { state: { literal: "listening" } } },
            { fields: { state: { literal: "thinking" } } },
            { fields: { state: { literal: "speaking" } } },
            { fields: { state: { literal: "muted" } } },
            {
                fields: {
                    state: { literal: "link_code" },
                    user_code: { type: "string" },
                    verification_uri: { type: "string" },
                    expires_at_unix: { type: "u64" },
                },
            },
            {
                fields: {
                    state: { literal: "approval" },
                    id: { type: "string" },
                    prompt: { type: "string" },
                    accept_label: { type: "string" },
                    deny_label: { type: "string" },
                },
            },
            {
                fields: {
                    state: { literal: "emotion" },
                    name: { type: "string" },
                    frame_ms: { type: "u64" },
                    frames: { array: { type: "string" } },
                },
            },
            {
                fields: {
                    state: { literal: "error" },
                    message: { type: "string" },
                    relink: { type: "boolean" },
                },
            },
        ],
    },
    UiAction: {
        variants: [
            {
                fields: {
                    action: { literal: "tap" },
                    target: { type: "string" },
                },
            },
            {
                fields: {
                    action: { literal: "approve" },
                    id: { type: "string" },
                },
            },
            { fields: { action: { literal: "deny" }, id: { type: "string" } } },
            { fields: { action: { literal: "relink" } } },
            { fields: { action: { literal: "dismiss" } } },
        ],
    },
    ScreenFrame: {
        fields: { seq: { type: "u64" }, state: { ref: "UiState" } },
    },
    MouthBatch: {
        fields: {
            sent_ms: { type: "u64" },
            samples: { array: { ref: "MouthSample" } },
        },
    },
    MouthSample: {
        fields: {
            at_ms: { type: "u64" },
            open: { type: "u8" },
            vowel: { ref: "MouthVowel" },
        },
    },
    MouthVowel: {
        variants: [
            { literal: "none" },
            { literal: "a" },
            { literal: "i" },
            { literal: "u" },
            { literal: "e" },
            { literal: "o" },
        ],
    },
} as const;
