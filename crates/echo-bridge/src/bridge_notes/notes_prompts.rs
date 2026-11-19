//! Prompt tool contract and fixed decoding. The prompt owner supplies all IO.
use super::Receipt;
use crate::bridge_voice::prompts;
use echo_agent_api::ToolSpec;
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn specs() -> Vec<ToolSpec> {
    [
        ("prompt_read", "Read a voice or runner base prompt.", false),
        ("prompt_write", "Replace a nonblank prompt of at most 32768 UTF-8 bytes; applies next conversation.", true),
    ].into_iter().map(|(name, description, write)| {
        let mut properties = json!({"which":{"type":"string","enum":["voice","runner"]}});
        let mut required = vec!["which"];
        if write {
            properties["content"] = json!({"type":"string"});
            required.push("content");
        }
        ToolSpec {
            name: name.into(), description: description.into(),
            input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }
    }).collect()
}

pub(super) fn execute(home: &Path, name: &str, args: &Value) -> Result<Receipt, &'static str> {
    let object = args.as_object().ok_or("invalid_arguments")?;
    let write = name == "prompt_write";
    if object.len() != if write { 2 } else { 1 } {
        return Err("invalid_arguments");
    }
    let which = object
        .get("which")
        .and_then(Value::as_str)
        .filter(|which| matches!(*which, "voice" | "runner"))
        .ok_or("invalid_arguments")?;
    let content = if write {
        let text = object
            .get("content")
            .and_then(Value::as_str)
            .ok_or("invalid_arguments")?;
        prompts::write_base(home, which, text)?;
        json!({"which":which,"bytes":text.len(),"applies":"next_conversation"})
    } else {
        let text = prompts::read_base(home, which)?;
        json!({"which":which,"bytes":text.len(),"content":text})
    };
    Ok(Receipt {
        bytes: content["bytes"].as_u64().ok_or("io")?,
        path: Some(which.into()),
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bridge_notes::Notes, bridge_trace::Trace};
    use echo_config::{AgentTools, HomeConfig};

    fn provider(home: &Path, trace: Trace) -> Notes {
        Notes::new(
            Some(&HomeConfig {
                dir: home.into(),
                agent_tools: AgentTools {
                    notes: false,
                    prompts: true,
                },
            }),
            trace,
        )
    }

    #[test]
    fn agent_tools_prompt_schema_arguments_effective_defaults_and_first_write() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let notes = provider(&home, Trace::open(&root.path().join("trace"), "api_key").unwrap());
        let specs = notes.specs();
        assert_eq!(
            specs.iter().map(|spec| spec.name.as_str()).collect::<Vec<_>>(),
            ["prompt_read", "prompt_write"]
        );
        assert_eq!(
            specs[0].input_schema,
            json!({"type":"object","properties":{"which":{"type":"string","enum":["voice","runner"]}},"required":["which"],"additionalProperties":false})
        );
        for args in [
            json!({}),
            json!({"which":"other"}),
            json!({"which":1}),
            json!({"which":"voice","extra":true}),
        ] {
            let result = notes.call("prompt_read", &args, "1:t1:u1:c").unwrap();
            assert_eq!(result.content["error"], "invalid_arguments");
            assert!(result.is_error);
        }
        for args in [
            json!({"which":"voice"}),
            json!({"which":"voice","content":1}),
            json!({"which":"voice","content":"x","extra":true}),
        ] {
            let result = notes.call("prompt_write", &args, "1:t1:u1:c").unwrap();
            assert_eq!(result.content["error"], "invalid_arguments");
            assert!(result.is_error);
        }
        for (which, text) in [
            ("voice", echo_config::LIVE_INSTRUCTIONS),
            ("runner", echo_config::RUNNER_INSTRUCTIONS),
        ] {
            let result = notes.call("prompt_read", &json!({"which":which}), "1:t1:u1:c").unwrap();
            assert_eq!(result.content, json!({"which":which,"content":text,"bytes":text.len()}));
            assert!(!result.is_error);
        }
        assert!(!home.exists());
        let result = notes
            .call("prompt_write", &json!({"which":"voice","content":"new"}), "1:t1:u1:c")
            .unwrap();
        assert_eq!(
            result.content,
            json!({"which":"voice","bytes":3,"applies":"next_conversation"})
        );
        assert!(!result.is_error);
        assert!(!home.join("prompts/.history").exists());
        assert_eq!(prompts::read_base(&home, "voice"), Ok("new".into()));
    }

    #[test]
    fn agent_tools_prompt_trace_failure_refuses_before_any_effect() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let notes = provider(&home, Trace::open(Path::new("/dev/full"), "api_key").unwrap());
        assert!(matches!(
            notes.call(
                "prompt_write",
                &json!({"which":"voice","content":"private"}),
                "1:t1:u1:c"
            ),
            Err(echo_agent_api::ToolError::Transport)
        ));
        assert!(!home.exists());
    }
}
