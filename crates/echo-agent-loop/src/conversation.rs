use crate::{ModelCredential, transport::Transport};
use echo_agent_api::{Task, ToolHost, ToolResult, ToolSpec};
use echo_config::AgentConfig;
use echo_contracts::ids::TaskId;
use serde_json::{Value, json};
use std::collections::HashSet;

type Failure = &'static str;
const INVALID: Failure = "invalid model response";

pub(crate) fn request(model: &str, history: &[Value], catalog: &[ToolSpec]) -> Value {
    let tools: Vec<_> = catalog
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            json!({
                "type":"function", "name": format!("tool_{index}"), "description":tool.description,
                "parameters":tool.input_schema, "strict":false,
            })
        })
        .collect();
    json!({"model":model, "stream":true, "input":history, "tools":tools,
        "tool_choice":"auto", "parallel_tool_calls":false, "reasoning":null,
        "store":false, "include":["reasoning.encrypted_content"]})
}

pub(crate) async fn run<H: ToolHost, C: ModelCredential>(
    id: &TaskId,
    task: Task,
    config: &AgentConfig,
    transport: &Transport,
    host: &H,
    credential: &C,
) -> Result<String, Failure> {
    let catalog = host.list_tools().await.map_err(|_| "tool catalog unavailable")?;
    let model = config.model.as_deref().ok_or("model credential unavailable")?;
    let mut history = vec![
        json!({"type":"message", "role":"developer", "content":[{"type":"input_text", "text":task.instructions}]}),
        json!({"type":"message", "role":"user", "content":[{"type":"input_text", "text":task.context.to_string()}]}),
    ];
    let mut seen = HashSet::new();
    loop {
        let items = transport
            .request(credential, &request(model, &history, &catalog))
            .await?;
        let calls = calls(&items, &catalog, &seen)?;
        if calls.is_empty() {
            return final_text(&items);
        }
        history.extend(items);
        for call in calls {
            if seen.len() as u64 >= u64::from(task.max_tool_calls.min(config.max_tool_calls)) {
                return Err("tool call limit reached");
            }
            seen.insert(call.id.clone());
            let key = format!("{id}:{}", call.id);
            let result = host
                .call_tool(&call.name, call.args, &key)
                .await
                .unwrap_or_else(|error| ToolResult {
                    content: json!({"error":error.to_string()}),
                    is_error: true,
                });
            history.push(json!({"type":"function_call_output", "call_id":call.id, "output":serde_json::to_string(&result).map_err(|_| INVALID)?}));
        }
    }
}

struct Call {
    id: String,
    name: String,
    args: Value,
}

fn calls(items: &[Value], catalog: &[ToolSpec], seen: &HashSet<String>) -> Result<Vec<Call>, Failure> {
    let mut ids = seen.clone();
    let mut calls = Vec::new();
    for item in items {
        match item["type"].as_str().ok_or(INVALID)? {
            "function_call" => {
                let id = item["call_id"].as_str().filter(|id| !id.is_empty()).ok_or(INVALID)?;
                if !ids.insert(id.into()) {
                    return Err(INVALID);
                }
                let alias = item["name"].as_str().ok_or(INVALID)?;
                let tool = catalog
                    .iter()
                    .enumerate()
                    .find(|(index, _)| alias == format!("tool_{index}"))
                    .map(|(_, tool)| tool)
                    .ok_or(INVALID)?;
                let args: Value =
                    serde_json::from_str(item["arguments"].as_str().ok_or(INVALID)?).map_err(|_| INVALID)?;
                if !args.is_object() {
                    return Err(INVALID);
                }
                calls.push(Call {
                    id: id.into(),
                    name: tool.name.clone(),
                    args,
                });
            }
            "message" | "reasoning" => (),
            _ => return Err(INVALID),
        }
    }
    Ok(calls)
}

fn final_text(items: &[Value]) -> Result<String, Failure> {
    let mut text = String::new();
    for item in items.iter().filter(|item| item["type"] == "message") {
        if item["role"] != "assistant" {
            return Err(INVALID);
        }
        for part in item["content"].as_array().ok_or(INVALID)? {
            if part["type"] == "output_text" {
                text.push_str(part["text"].as_str().ok_or(INVALID)?);
            }
        }
    }
    if text.is_empty() { Err(INVALID) } else { Ok(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_preserve_arbitrary_host_names_and_exact_schemas() {
        let catalog = vec![
            ToolSpec {
                name: "tool_1".into(),
                description: "First".into(),
                input_schema: json!({"type":"object", "properties":{"value":{"type":"integer"}}}),
            },
            ToolSpec {
                name: "server.tools/second".into(),
                description: "Second".into(),
                input_schema: json!({"oneOf":[{"type":"object"}, {"type":"null"}]}),
            },
        ];
        let body = request(" fixture-model ", &[], &catalog);
        assert_eq!(body["model"], " fixture-model ");
        assert_eq!(body["input"], json!([]));
        assert_eq!(
            body["tools"],
            json!([
                {"type":"function", "name":"tool_0", "description":"First", "parameters":catalog[0].input_schema, "strict":false},
                {"type":"function", "name":"tool_1", "description":"Second", "parameters":catalog[1].input_schema, "strict":false},
            ])
        );
        let decoded = calls(
            &[json!({"type":"function_call", "name":"tool_1", "call_id":"call-1", "arguments":"{\"value\":3}"})],
            &catalog,
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].name, "server.tools/second");
        assert_eq!(decoded[0].args, json!({"value":3}));
        assert_eq!(decoded[0].id, "call-1");
    }

    #[test]
    fn final_text_uses_only_assistant_output_blocks_in_order() {
        let items = vec![
            json!({"type":"reasoning", "encrypted_content":"opaque"}),
            json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"one"}, {"type":"annotation", "text":"ignored"}]}),
            json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"two"}]}),
        ];
        assert_eq!(final_text(&items), Ok("onetwo".into()));
    }
}
