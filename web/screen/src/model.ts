// Generated from schemas/screen/*.json by tools/screen-schema.mjs.
// Schema SHA-256: e11dc9f8cb6939a6b55ebc10bd0c5cb1dd0526924d0f635c42b712af9c68ab48
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
    | { state: "error"; message: string; relink: boolean }
    | {
          state: "open_ai_relink";
          message: string;
          starting: boolean;
          muted: boolean;
      }
    | {
          state: "open_ai_signin";
          attempt_id: string;
          user_code: string;
          verification_uri: string;
          expires_at_unix: bigint;
          muted: boolean;
      };
export type UiAction =
    | { action: "tap"; target: string }
    | { action: "approve"; id: string }
    | { action: "deny"; id: string }
    | { action: "relink" }
    | { action: "dismiss" }
    | { action: "cancel_open_ai_signin"; attempt_id: string }
    | { action: "renew_open_ai_signin"; attempt_id: string };
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
            {
                fields: {
                    state: { literal: "open_ai_relink" },
                    message: { type: "string" },
                    starting: { type: "boolean" },
                    muted: { type: "boolean" },
                },
            },
            {
                fields: {
                    state: { literal: "open_ai_signin" },
                    attempt_id: { type: "string" },
                    user_code: { type: "string" },
                    verification_uri: { type: "string" },
                    expires_at_unix: { type: "u64" },
                    muted: { type: "boolean" },
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
            {
                fields: {
                    action: { literal: "cancel_open_ai_signin" },
                    attempt_id: { type: "string" },
                },
            },
            {
                fields: {
                    action: { literal: "renew_open_ai_signin" },
                    attempt_id: { type: "string" },
                },
            },
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
