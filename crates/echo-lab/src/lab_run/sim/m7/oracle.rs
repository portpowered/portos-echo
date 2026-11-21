//! Join raw independent observers before publishing any M7 measurements.
use super::world::{DEADLINE, SUCCESS, World};
use serde_json::{Value, json};

fn equal(label: &str, actual: &Value, expected: &Value) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("m7 {label}: expected {expected}; observed {actual}"))
    }
}

fn one<'a>(records: &'a [Value], event: &str) -> Result<&'a Value, String> {
    let entries: Vec<_> = records.iter().filter(|e| e["event"] == event).collect();
    if entries.len() != 1 {
        return Err(format!("m7 {event}: expected 1; observed {}", entries.len()));
    }
    Ok(entries[0])
}

pub fn observe(world: &World, negative: bool) -> Result<Value, String> {
    let records = &world.trace.records;
    let created = one(records, "delegation.created")?;
    let run = required(created, "run_id")?;
    let delegation = required(created, "delegation_id")?;
    equal("completed run", &world.completion["run_id"], &json!(run))?;
    let task = required(one(records, "muse.submit")?, "task_id")?;
    let progress = records.iter().filter(|event| event["event"] == "muse.progress").count();
    if progress == 0 {
        return Err("m7: expected at least one Progress".into());
    }
    identities(records, run, delegation, task)?;
    let terminal = one(records, "muse.terminal")?;
    let status = if negative { "failed" } else { "completed" };
    equal("terminal status", &terminal["status"], &json!(status))?;
    if negative {
        equal("retryable", &terminal["retryable"], &json!(false))?;
    }
    native_tool(records, run, delegation, status)?;
    ordering(records)?;
    let (session, deadline) = gadget(world, task, delegation)?;
    let report = report(&world.muse.reports, task, negative)?;
    let text = dialogue(world, delegation, negative)?;
    playback(world, run)?;
    Ok(
        json!({"event":"m7.case","case":if negative {"no_report"} else {"success"},
        "run_id":run,"delegation_id":delegation,"task_id":task,"gadget_session_id":session,
        "report_task_id":report,"transcript":text,"delegation_count":1,"muse_submit_count":1,
        "progress_count":progress,"terminal_count":1,"completed_count":usize::from(!negative),
        "failed_count":usize::from(negative),"spoken_reply_count":1,"native_tool_call_count":1,
        "native_append_count":1,"legacy_dispatch_count":0,"model_tool_output":world.dialogue["tool_output"],
        "gadget_submit_count":world.muse.calls.len(),"report_accepted_count":world.muse.reports.len(),
        "report_attempt_count":world.muse.reports.len(),"reply_pcm_bytes":world.playback["pcm_bytes"],
        "audibility":world.audibility,"ui_states":world.ui_states,
        "client_tool_call_count":world.tool_calls,"responses_request_count":2,
        "retryable":if negative {json!(false)} else {Value::Null},"deadline_seconds":deadline}),
    )
}

fn required<'a>(record: &'a Value, key: &str) -> Result<&'a str, String> {
    record[key]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("m7: missing {key}"))
}

fn native_tool(records: &[Value], run: &str, delegation: &str, status: &str) -> Result<(), String> {
    let request = one(records, "item/tool/call")?;
    let result = one(records, "delegation.tool")?;
    equal("native tool name", &request["name"], &json!("ask_muse"))?;
    equal("native request run", &request["run_id"], &json!(run))?;
    equal("native call ID", &request["call_id"], &json!("tool-local"))?;
    for kind in ["name", "run_id", "thread_id", "turn_id", "call_id", "request_id"] {
        if request[kind].is_null() || result[kind] != request[kind] {
            return Err(format!("m7: native tool receipt mismatch for {kind}"));
        }
    }
    equal("native delegation ID", &result["delegation_id"], &json!(delegation))?;
    equal("native tool kind", &result["kind"], &json!("agent"))?;
    equal("native tool status", &result["status"], &json!(status))
}

fn identities(records: &[Value], run: &str, delegation: &str, task: &str) -> Result<(), String> {
    for event in ["muse.submit", "muse.progress", "muse.terminal"] {
        for entry in records.iter().filter(|e| e["event"] == event) {
            equal("raw run ID", &entry["run_id"], &json!(run))?;
            equal("raw delegation ID", &entry["delegation_id"], &json!(delegation))?;
            equal("raw task ID", &entry["task_id"], &json!(task))?;
        }
    }
    Ok(())
}

fn playback(world: &World, run: &str) -> Result<(), String> {
    let records = &world.trace.records;
    let reply = one(records, "reply.played")?;
    equal("reply run", &reply["run_id"], &json!(run))?;
    equal("reply playback", &reply["outcome"], &json!("played"))?;
    let audio = &world.audibility;
    if world.playback["pcm_bytes"] != 9600
        || world.playback["nonzero_bytes"].as_u64().is_none_or(|n| n == 0)
        || audio["pcm_bytes"] != 9600
        || audio["nonzero_samples"].as_u64().is_none_or(|n| n < 2400)
        || audio["audible_samples"].as_u64().is_none_or(|n| n < 2400)
        || audio["host"] != world.playback
    {
        return Err(format!(
            "m7: expected audible 9600-byte reply; observed {audio}, host {}",
            world.playback
        ));
    }
    let states: Vec<_> = world.ui_states.iter().map(|state| state["state"].clone()).collect();
    equal(
        "screen sequence",
        &json!(states),
        &json!([
            "ready",
            "listening",
            "connected",
            "thinking",
            "speaking",
            "connected",
            "ready"
        ]),
    )?;
    equal("client tool calls", &json!(world.tool_calls), &json!(0))?;
    if records.iter().any(|e| {
        matches!(
            e["event"].as_str(),
            Some("tools/call" | "session.delegation.created" | "session.commentary.append")
        )
    }) {
        return Err("m7: unexpected client execution".into());
    }
    Ok(())
}

fn ordering(records: &[Value]) -> Result<(), String> {
    let submit = records
        .iter()
        .position(|e| e["event"] == "muse.submit")
        .ok_or("m7: missing submit")?;
    let terminal = records
        .iter()
        .position(|e| e["event"] == "muse.terminal")
        .ok_or("m7: missing terminal")?;
    if records
        .iter()
        .enumerate()
        .any(|(i, e)| e["event"] == "muse.progress" && (i <= submit || i >= terminal))
    {
        return Err("m7: progress outside admitted task window".into());
    }
    let mut previous = None;
    for event in [
        "delegation.created",
        "item/tool/call",
        "muse.submit",
        "muse.progress",
        "muse.terminal",
        "delegation.tool",
        "reply.played",
    ] {
        let index = records
            .iter()
            .position(|e| e["event"] == event)
            .ok_or("m7: missing ordered event")?;
        if previous.is_some_and(|last| index <= last) {
            return Err(format!("m7: {event} is out of order"));
        }
        previous = Some(index);
    }
    Ok(())
}

fn gadget(world: &World, task: &str, delegation: &str) -> Result<(String, u64), String> {
    let calls = &world.muse.calls;
    if calls.len() != 1 {
        return Err(format!("m7 gadget submits: expected 1; observed {}", calls.len()));
    }
    let record = &world.muse.admission;
    equal("daemon task ID", &record["task_id"], &json!(task))?;
    equal(
        "daemon delegation ID",
        &record["task"]["delegation_id"],
        &json!(delegation),
    )?;
    let deadline = record["task"]["deadline_seconds"]
        .as_u64()
        .filter(|seconds| (1..=10).contains(seconds))
        .ok_or("m7: invalid admitted deadline")?;
    equal(
        "daemon absolute deadline",
        &record["deadline_unix_seconds"],
        &json!(100 + deadline),
    )?;
    let argv = calls[0]["argv"]
        .as_array()
        .filter(|a| a.len() == 6)
        .ok_or("m7: invalid gadget argv")?;
    equal("gadget command", &argv[0], &json!("send-user-msg"))?;
    equal("gadget session option", &argv[1], &json!("--session-id"))?;
    equal("gadget wait option", &argv[3], &json!("--wait"))?;
    equal("gadget wait", &argv[4], &json!("1"))?;
    equal("gadget stdin", &calls[0]["stdin"], &json!(""))?;
    let session = argv[2]
        .as_str()
        .filter(|s| s.len() == 36 && s.as_bytes()[14] == b'4')
        .ok_or("m7: invalid gadget session")?;
    equal("gadget-to-daemon session", &record["session_id"], &json!(session))?;
    let expected = format!(
        "Run the test job\nContext: null\nTask ID: {task}\nWhen you are finished, call the echo.report.result command with task_id {task}, status completed or failed, and a short summary. Do not report success any other way."
    );
    equal("gadget request and correlation", &argv[5], &json!(expected))?;
    Ok((session.into(), deadline))
}

fn report(reports: &[Value], task: &str, negative: bool) -> Result<String, String> {
    if reports.len() != usize::from(!negative) {
        return Err(format!(
            "m7 report attempts: expected {}; observed {}",
            usize::from(!negative),
            reports.len()
        ));
    }
    if negative {
        return Ok(String::new());
    }
    equal("report exit", &reports[0]["exit"], &json!(0))?;
    equal("report stderr", &reports[0]["stderr"], &json!(""))?;
    let ack: Value = serde_json::from_str(reports[0]["stdout"].as_str().ok_or("m7: missing handler stdout")?)
        .map_err(|_| "m7: invalid report acknowledgement")?;
    equal("report acknowledgement", &ack, &json!({"accepted":true,"task_id":task}))?;
    Ok(task.into())
}

fn dialogue(world: &World, delegation: &str, negative: bool) -> Result<String, String> {
    let provider = &world.dialogue;
    let expected = if negative { DEADLINE } else { SUCCESS };
    equal("native delegations", &provider["delegations"], &json!([delegation]))?;
    equal("native appends", &provider["appends"], &json!([delegation]))?;
    equal("loopback sideband", &provider["sidebands"], &json!(1))?;
    equal("external provider requests", &provider["external"], &json!(0))?;
    equal("model-visible output", &provider["tool_output"], &json!(expected))?;
    let models = provider["models"]
        .as_array()
        .filter(|models| models.len() == 2)
        .ok_or("m7: expected exactly two model requests")?;
    for (index, model) in models.iter().enumerate() {
        equal(
            "model catalog",
            &model["tools"],
            &json!(["extend_session", "end_session", "ask_muse"]),
        )?;
        equal("model output count", &model["outputs"], &json!(index))?;
    }
    equal(
        "spoken transcript",
        &provider["transcripts"],
        &json!([
            {"role":"user","text":"Computer, ask Muse to run the test job"},
            {"role":"assistant","text":expected}
        ]),
    )?;
    Ok(expected.into())
}

#[cfg(test)]
mod tests;
