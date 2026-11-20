//! Local Port OS scope admission and a deterministic two-light home.
use super::{Shared, mcp_tokens};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

const ROOM: &str = "group-twin-room";
const MESSAGE: &str = "power/turn-off";
const TOOLS: [(&str, &str); 11] = [
    ("control", "message:send"),
    ("endpoint_query", "endpoint:read"),
    ("endpoint_show", "endpoint:read"),
    ("endpoint_manage", "endpoint:write"),
    ("group_query", "group:read"),
    ("group_manage", "group:manage"),
    ("guide", ""),
    ("subscription_list", "subscription:read"),
    ("subscription_inspect", "subscription:read"),
    ("subscription_cancel", "subscription:write"),
    ("subscription_label_edit", "subscription:write"),
];

pub(super) struct Home {
    calls: Vec<Value>,
    pub(super) scope_denials: Vec<Value>,
    pub(super) fault: Option<super::HomeFault>,
    lights_on: bool,
}

impl Default for Home {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            scope_denials: Vec::new(),
            fault: None,
            lights_on: true,
        }
    }
}

impl Home {
    pub(super) fn snapshot(&self) -> Value {
        json!({"calls":self.calls,"lights_on":[self.lights_on,self.lights_on],"plug_on":true})
    }

    fn call(&mut self, name: &str, args: &Value) -> Value {
        if self.calls.len() >= 128 {
            return content(&json!({"error":"capacity"}), true);
        }
        self.calls.push(json!({"name":name,"arguments":args}));
        match name {
            "group_query" => self.group_query(),
            "endpoint_query" | "endpoint_show" => content(
                &json!({"endpoints":[
                    {"id":"light-a","name":"Twin one","power":self.lights_on,"messages":[MESSAGE]},
                    {"id":"light-b","name":"Twin two","power":self.lights_on,"messages":[MESSAGE]},
                    {"id":"plug-unrelated","name":"Unrelated plug","power":true}
                ]}),
                false,
            ),
            "control" if args["id"] == ROOM && args["message"] == MESSAGE && args["payload"] == json!({}) => {
                self.lights_on = false;
                content(&json!({"status":"accepted"}), false)
            }
            "guide" => content(
                &json!({"text":"Resolve the room, copy its ID and message, then control once."}),
                false,
            ),
            _ => content(&json!({"error":"unsupported synthetic operation"}), true),
        }
    }

    fn group_query(&self) -> Value {
        use super::HomeFault::{AmbiguousRoom, NoRoom};
        let room = json!({"id":ROOM,"name":"Twin room","messages":[MESSAGE],"endpointIds":["light-a","light-b"]});
        let groups = match self.fault {
            Some(NoRoom) => vec![],
            Some(AmbiguousRoom) => vec![
                room,
                json!({"id":"group-twin-other","name":"Twin room",
                "messages":[MESSAGE],"endpointIds":[]}),
            ],
            _ => vec![room],
        };
        content(&json!({"groups":groups}), false)
    }
}

pub(super) async fn request(State(shared): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let Ok(mut state) = shared.lock() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if state.resource != format!("{}/mcp", state.issuer)
        || headers.contains_key("dpop")
        || !bearer.is_some_and(|v| mcp_tokens::accepts(&state, v))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let method = body["method"].as_str().unwrap_or_default();
    let name = body["params"]["name"].as_str().unwrap_or_default();
    if let Some(scope) = required_scope(method, name)
        && !state.scopes.split(' ').any(|granted| granted == scope)
    {
        if method == "tools/call" && state.home.scope_denials.len() < 128 {
            state.home.scope_denials.push(json!({
                "name":name,"arguments":body["params"]["arguments"],"scope":scope
            }));
        }
        return (
            StatusCode::FORBIDDEN,
            [(
                "www-authenticate",
                format!("Bearer error=\"insufficient_scope\", scope=\"{scope}\""),
            )],
            Json(json!({"error":"insufficient_scope"})),
        )
            .into_response();
    }
    let result = match method {
        "initialize" => json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},
            "serverInfo":{"name":"portos-synthetic","version":"1.0"}}),
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "tools/list" => json!({"tools":catalog()}),
        "tools/call" if TOOLS.iter().any(|(tool, _)| *tool == name) => {
            let arguments = &body["params"]["arguments"];
            if name == "control" && state.home.fault == Some(super::HomeFault::ControlUnavailable) {
                if state.home.calls.len() < 128 {
                    state.home.calls.push(json!({"name":name,"arguments":arguments}));
                }
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            state.home.call(name, arguments)
        }
        _ => {
            return Json(json!({"jsonrpc":"2.0","id":body["id"],
            "error":{"code":-32601,"message":"unsupported synthetic method"}}))
            .into_response();
        }
    };
    Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result})).into_response()
}

pub(super) fn catalog() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|(name, _)| json!({"name":name,"description":"Synthetic local home tool","inputSchema":schema(name)}))
        .collect()
}

fn schema(name: &str) -> Value {
    let mut schema = json!({"type":"object","properties":{},"additionalProperties":false});
    match name {
        "group_query" | "endpoint_query" | "endpoint_show" => {
            let predicate = json!({"type":"object","additionalProperties":false,"properties":{"eq":{"type":"string"}}});
            schema["properties"]["query"] = json!({"type":"object","additionalProperties":false,
                "properties":{"name":predicate,"role":predicate}});
        }
        "control" => {
            schema["properties"] = json!({"id":{"type":"string"},"message":{"type":"string"},
                "payload":{"type":"object","properties":{},"additionalProperties":false},
                "idempotencyKey":{"type":"string"},"waitForResult":{"type":"boolean"}});
            schema["required"] = json!(["id", "message", "payload"]);
        }
        _ => {}
    }
    schema
}

fn required_scope(method: &str, name: &str) -> Option<&'static str> {
    match method {
        "tools/call" => TOOLS
            .iter()
            .find(|(tool, _)| *tool == name)
            .map(|(_, scope)| *scope)
            .filter(|s| !s.is_empty()),
        "events/list" => Some("subscription:read"),
        "events/subscribe" | "events/unsubscribe" => Some("subscription:write"),
        _ => None,
    }
}

fn content(value: &Value, failed: bool) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"isError":failed})
}
