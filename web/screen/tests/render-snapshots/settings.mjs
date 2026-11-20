// Synthetic Settings data for the render snapshots; no real account.
const scopes = ["endpoint:read", "endpoint:write", "message:send"];
const tools = ["control", "endpoint_query", "endpoint_show", "guide"];

export function settingsBody(portos) {
    return {
        body: {
            version: 2,
            settings: {
                voice: {
                    session_listen_enabled: true,
                    session_listen_seconds: 15,
                },
                codex: {
                    voice: "marin",
                    web_search: "cached",
                    delegate_model: "synthetic-runner",
                    delegate_effort: "low",
                },
                agent: {
                    daily_request_cap: "1000",
                    max_tool_calls: 6,
                    task_deadline_seconds: 45,
                },
                home: { agent_tools: ["notes", "prompts"] },
            },
            allowed_voices: ["default", "marin", "cedar"],
            prompts: {
                voice: "Voice instructions",
                runner: "Runner instructions",
            },
            bridge_version: "0.1.0",
            restart_pending: false,
            warnings: [],
            portos: {
                link_status: portos,
                operation_state: "idle",
                link_waiting_for_restart: false,
                scope_choices: scopes,
                tool_choices: tools,
                server: {
                    name: "portos",
                    url: "https://127.0.0.1/mcp",
                    requested_scopes: ["endpoint:read", "message:send"],
                    granted_scopes:
                        portos === "linked"
                            ? ["endpoint:read", "message:send"]
                            : [],
                    allow_tools: ["endpoint_query", "control"],
                },
            },
        },
        openAI: {
            version: 2,
            link_status: portos === "linked" ? "linked" : "link_required",
            operation_state: "idle",
        },
    };
}
