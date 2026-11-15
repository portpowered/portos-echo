// Generated from schemas/screen/*.json by tools/screen-schema.mjs.
// Schema SHA-256: 6c7981116e3f42675d6d1101d2b6095386380de8b42a8e59a65d1ae3b4388c0e
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
} as const;
