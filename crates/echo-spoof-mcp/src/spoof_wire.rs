//! Exact deterministic tool catalog and argument validation.
use serde_json::{Value, json};

#[must_use]
pub fn catalog() -> Value {
    json!([
        {"name":"test.echo","description":"Echo text","inputSchema":schema("text",&json!({"type":"string"}))},
        {"name":"test.wait","description":"Wait for milliseconds","inputSchema":schema("ms",&json!({"type":"integer","minimum":0}))},
        {"name":"test.requires_approval","description":"Require approval","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"ui.show_emotion","description":"Show emotion","inputSchema":schema("name",&json!({"type":"string"}))}
    ])
}

fn schema(key: &str, kind: &Value) -> Value {
    json!({"type":"object","properties":{key:kind},"required":[key],"additionalProperties":false})
}

pub(crate) fn valid_args(name: &str, args: &Value) -> bool {
    let Some(object) = args.as_object() else {
        return false;
    };
    match name {
        "test.echo" => object.len() == 1 && args["text"].is_string(),
        "test.wait" => object.len() == 1 && args["ms"].as_u64().is_some(),
        "test.requires_approval" => object.is_empty(),
        "ui.show_emotion" => object.len() == 1 && args["name"].is_string(),
        _ => false,
    }
}

pub(crate) async fn execute(name: &str, args: &Value) -> Value {
    let text = match name {
        "test.echo" => args["text"].as_str().unwrap_or_default().to_owned(),
        "test.wait" => {
            let ms = args["ms"].as_u64().unwrap_or_default();
            wait_millis(ms).await;
            format!("waited {ms} ms")
        }
        "test.requires_approval" => "approval_required".into(),
        "ui.show_emotion" if args["name"] == "happy" => json!({
            "state":"emotion", "name":"happy", "frame_ms":80,
            "frames":["(^_^)", "(^o^)"]
        })
        .to_string(),
        _ => args["name"].as_str().unwrap_or_default().to_owned(),
    };
    json!({"content":[{"type":"text","text":text}],"isError":name=="test.requires_approval"})
}

pub(crate) fn valid_envelope(value: &Value) -> bool {
    if value["jsonrpc"] != "2.0" {
        return false;
    }
    if value["method"] == "notifications/initialized" {
        return value.get("id").is_none();
    }
    if value["id"].as_u64().is_none_or(|id| id == 0) {
        return false;
    }
    let params = &value["params"];
    match value["method"].as_str() {
        Some("initialize") => {
            params["protocolVersion"] == "2025-03-26"
                && params["capabilities"]
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty)
                && params["clientInfo"]["name"].is_string()
                && params["clientInfo"]["version"].is_string()
        }
        Some("tools/list") => params
            .as_object()
            .is_some_and(|p| p.is_empty() || (p.len() == 1 && params["cursor"].is_string())),
        Some("tools/call") => params["name"].is_string() && params["arguments"].is_object(),
        _ => false,
    }
}

pub(crate) fn rpc(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

pub(crate) fn invalid_args(id: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"invalid tool arguments"}})
}

// Chunk unsigned wire durations so even u64::MAX never overflows an Instant.
pub(crate) async fn wait_millis(mut ms: u64) {
    while ms > 60_000 {
        tokio::time::sleep(std::time::Duration::from_millis(60_000)).await;
        ms -= 60_000;
    }
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}
