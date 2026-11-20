//! Observed pinned item shapes; wrapper items never imply nested effects.
use crate::Error;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use tokio::time::Instant;

#[derive(Default)]
pub(super) struct Tools {
    starts: HashMap<String, Instant>,
    finished: HashSet<String>,
    pub counts: BTreeMap<&'static str, u64>,
}

impl Tools {
    pub fn observe(&mut self, method: &str, item: &Value, names: &[(String, String)]) -> Result<Option<Value>, Error> {
        let Some(id) = item["id"].as_str().filter(|id| valid(id)) else {
            return Ok(None);
        };
        let Some((kind, name)) = identity(item, names) else {
            return Ok(None);
        };
        if self.finished.contains(id) {
            return Ok(None);
        }
        if self.starts.len() + self.finished.len() >= 1024 && !self.starts.contains_key(id) {
            return Err(Error::Trace);
        }
        if method == "item/started" {
            self.starts.entry(id.into()).or_insert_with(Instant::now);
            return Ok(None);
        }
        if method != "item/completed" {
            return Ok(None);
        }
        let Some(status) = status(item) else { return Ok(None) };
        let mut fields = json!({"item_id":id,"kind":kind,"name":name,"status":status});
        if let Some(duration) = item["durationMs"].as_u64().or_else(|| {
            self.starts
                .get(id)
                .map(|start| u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX))
        }) {
            fields["duration_ms"] = duration.into();
        }
        payloads(&mut fields, item);
        if kind == "mcp"
            && let Some(server) = item["server"].as_str().filter(|server| valid(server))
        {
            fields["server"] = server.into();
        }
        self.starts.remove(id);
        self.finished.insert(id.into());
        *self.counts.entry(kind).or_default() += 1;
        Ok(Some(fields))
    }
}

fn valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

fn identity<'a>(item: &'a Value, names: &'a [(String, String)]) -> Option<(&'static str, &'a str)> {
    match item["type"].as_str()? {
        "webSearch" => Some(("web_search", "web_search")),
        "functionCallOutput" if item["name"] == "web.run" => Some(("web_search", "web.run")),
        "dynamicToolCall" => {
            let alias = item["tool"].as_str()?;
            let (_, name) = names.iter().find(|(candidate, _)| candidate == alias)?;
            Some((local_kind(name), name))
        }
        "mcpToolCall" => Some(("mcp", item["tool"].as_str().filter(|name| valid(name))?)),
        "commandExecution" => Some(("command", "command")),
        // Unknown function names and native wrappers cannot invent completions.
        _ => None,
    }
}

fn local_kind(name: &str) -> &'static str {
    match name {
        "notes_read" | "notes_write" | "notes_list" | "prompt_read" | "prompt_write" => "agent",
        "extend_session" | "end_session" => "home",
        _ => "mcp",
    }
}

fn status(item: &Value) -> Option<&'static str> {
    match item["status"].as_str() {
        Some("completed") if item["success"] == false => Some("failed"),
        Some("completed") => Some("completed"),
        Some("failed") => Some("failed"),
        Some("declined") => Some("declined"),
        None if item["type"] == "webSearch" || item["type"] == "functionCallOutput" => Some("completed"),
        _ => None,
    }
}

fn payloads(fields: &mut Value, item: &Value) {
    match item["type"].as_str() {
        Some("webSearch") => {
            fields["request"] = json!({"query":item["query"],"action":item["action"]});
            if let Some(results) = item["results"].as_array() {
                fields["result_count"] = results.len().into();
                fields["response"] = json!({"results":results});
            }
        }
        Some("dynamicToolCall") => {
            fields["request"] = item["arguments"].clone();
            fields["response"] = json!({"contentItems":item["contentItems"],"success":item["success"]});
        }
        Some("mcpToolCall") => {
            fields["request"] = item["arguments"].clone();
            fields["response"] = json!({"result":item["result"],"error":item["error"]});
        }
        Some("commandExecution") => {
            fields["request"] = json!({"command":item["command"],"cwd":item["cwd"]});
            fields["response"] = json!({"output":item["aggregatedOutput"],"exit_code":item["exitCode"]});
        }
        Some("functionCallOutput") => fields["response"] = item["output"].clone(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn delegation_trace_tools_keep_status_duration_aliases_and_exact_counts() {
        let names = vec![
            ("test_echo__c1".into(), "test.echo".into()),
            ("notes_read".into(), "notes_read".into()),
        ];
        let mut tools = Tools::default();
        let cases = [
            (
                json!({"type":"dynamicToolCall","tool":"test_echo__c1","arguments":{"query":"seed"},"status":"failed","success":false}),
                "mcp",
                "test.echo",
                "failed",
            ),
            (
                json!({"type":"dynamicToolCall","tool":"notes_read","status":"completed","success":true}),
                "agent",
                "notes_read",
                "completed",
            ),
            (
                json!({"type":"mcpToolCall","tool":"test.wait","server":"spoof","status":"failed","error":{"message":"seed failure"},"durationMs":17}),
                "mcp",
                "test.wait",
                "failed",
            ),
            (
                json!({"type":"commandExecution","command":"fixture","status":"declined"}),
                "command",
                "command",
                "declined",
            ),
            (
                json!({"type":"functionCallOutput","name":"web.run","output":{"result":"seed"}}),
                "web_search",
                "web.run",
                "completed",
            ),
        ];
        for (index, (mut item, kind, name, status)) in cases.into_iter().enumerate() {
            item["id"] = format!("item-{index}").into();
            assert!(tools.observe("item/started", &item, &names).unwrap().is_none());
            tokio::time::advance(std::time::Duration::from_millis(7)).await;
            let row = tools.observe("item/completed", &item, &names).unwrap().unwrap();
            assert_eq!(row["kind"], kind);
            assert_eq!(row["name"], name);
            assert_eq!(row["status"], status);
            assert_eq!(row["duration_ms"], if index == 2 { 17 } else { 7 });
            if index == 2 {
                assert_eq!(row["server"], "spoof");
                assert_eq!(row["response"]["error"]["message"], "seed failure");
            }
            assert!(tools.observe("item/completed", &item, &names).unwrap().is_none());
        }
        assert_eq!(
            serde_json::to_value(tools.counts).unwrap(),
            json!({"mcp":2,"agent":1,"command":1,"web_search":1})
        );
    }

    #[test]
    fn delegation_trace_tools_do_not_invent_wrappers_counts_durations_or_sources() {
        let mut tools = Tools::default();
        for item in [
            json!({"id":"a","type":"dynamicToolCall","tool":"functions.exec","status":"completed"}),
            json!({"id":"b","type":"functionCallOutput","name":"functions.exec","output":{}}),
            json!({"id":"c","type":"webSearch","status":"inProgress"}),
            json!({"id":"unsafe\nid","type":"webSearch"}),
        ] {
            assert!(tools.observe("item/completed", &item, &[]).unwrap().is_none());
        }
        assert_eq!(tools.counts.len(), 0);
        let row = tools
            .observe(
                "item/completed",
                &json!({"id":"s","type":"webSearch","query":"seed"}),
                &[],
            )
            .unwrap()
            .unwrap();
        assert!(row.get("duration_ms").is_none());
        assert!(row.get("result_count").is_none());
        assert!(row.get("source_count").is_none());
        assert!(row.get("response").is_none());
        assert_eq!(row["request"], json!({"query":"seed","action":null}));
        assert_eq!(tools.counts["web_search"], 1);
    }
}
