//! Join raw independent observers before publishing any M7 measurements.
use super::world::{DEADLINE, SUCCESS, World};
use echo_live::fake::DialogueSent;
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
    let created = one(records, "session.delegation.created")?;
    let run = created["run_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("m7: missing raw run ID")?;
    let delegation = created["delegation_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("m7: missing raw delegation ID")?;
    equal("completed run", &world.completion["run_id"], &json!(run))?;
    let submitted = one(records, "muse.submit")?;
    let task = submitted["task_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("m7: missing raw task ID")?;
    let progress = records
        .iter()
        .filter(|e| e["event"] == "muse.progress")
        .collect::<Vec<_>>();
    if progress.is_empty() {
        return Err("m7: expected at least one Progress".into());
    }
    identities(records, run, delegation, task)?;
    let terminal = one(records, "muse.terminal")?;
    equal(
        "terminal status",
        &terminal["status"],
        &json!(if negative { "failed" } else { "completed" }),
    )?;
    if negative {
        equal("retryable", &terminal["retryable"], &json!(false))?;
    }
    let commentary = one(records, "session.commentary.append")?;
    equal(
        "commentary delegation",
        &commentary["delegation_id"],
        &json!(delegation),
    )?;
    equal("commentary run", &commentary["run_id"], &json!(run))?;
    ordering(records)?;
    let session = gadget(&world.muse.calls, task, delegation)?;
    let report = report(&world.muse.reports, task, negative)?;
    let text = dialogue(world, delegation, negative)?;
    playback(world, run)?;
    Ok(
        json!({"event":"m7.case","case":if negative {"no_report"} else {"success"},
        "run_id":run,"delegation_id":delegation,"task_id":task,"gadget_session_id":session,
        "report_task_id":report,"transcript":text,"delegation_count":1,"muse_submit_count":1,
        "progress_count":progress.len(),"terminal_count":1,"completed_count":usize::from(!negative),
        "failed_count":usize::from(negative),"spoken_reply_count":1,"commentary_count":1,
        "gadget_submit_count":world.muse.calls.len(),"report_accepted_count":world.muse.reports.len(),
        "report_attempt_count":world.muse.reports.len(),"reply_pcm_bytes":world.playback["pcm_bytes"],
        "audibility":world.audibility,"ui_states":world.ui_states,
        "client_tool_call_count":world.tool_calls,"responses_request_count":world.model_requests,
        "retryable":if negative {json!(false)} else {Value::Null},"deadline_seconds":10}),
    )
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
    let started = one(records, "reply.started")?;
    equal("reply start run", &started["run_id"], &json!(run))?;
    let reply = one(records, "reply.played")?;
    equal("reply run", &reply["run_id"], &json!(run))?;
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
        &json!(["ready", "listening", "connected", "speaking", "connected", "ready"]),
    )?;
    equal("client model requests", &json!(world.model_requests), &json!(0))?;
    equal("client tool calls", &json!(world.tool_calls), &json!(0))?;
    if records.iter().any(|e| e["event"] == "tools/call") {
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
        "session.delegation.created",
        "muse.submit",
        "muse.progress",
        "muse.terminal",
        "session.commentary.append",
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

fn gadget(calls: &[Value], task: &str, delegation: &str) -> Result<String, String> {
    if calls.len() != 1 {
        return Err(format!("m7 gadget submits: expected 1; observed {}", calls.len()));
    }
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
    let prompt = argv[5].as_str().ok_or("m7: missing gadget prompt")?;
    let prefix = "Execute the delegated request and report its result.\nContext: ";
    let suffix = format!(
        "\nTask ID: {task}\nWhen you are finished, call the echo.report.result command with task_id {task}, status completed or failed, and a short summary. Do not report success any other way."
    );
    let context = prompt
        .strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(&suffix))
        .ok_or("m7: gadget task ID/prompt mismatch")?;
    let context: Value = serde_json::from_str(context).map_err(|_| "m7: invalid delegated context")?;
    equal("gadget delegation ID", &context["delegation"]["id"], &json!(delegation))?;
    equal(
        "gadget delegation target",
        &context["delegation"]["target"],
        &json!("client"),
    )?;
    Ok(session.into())
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
    if world.dialogue.len() != 1 {
        return Err("m7: expected one Live call".into());
    }
    let call = &world.dialogue[0];
    if call.call_index != 0
        || call.delegation_id != delegation
        || call.failure.is_some()
        || call.rejected_commentary != 0
        || !call.reply_completed
    {
        return Err("m7: invalid or stale Live observation".into());
    }
    let sent = call
        .sent
        .iter()
        .filter_map(|sent| match sent {
            DialogueSent::Text(text) => serde_json::from_str::<Value>(text)
                .ok()
                .filter(|e| e["type"] == "response.audio_transcript.done"),
            DialogueSent::Audio(_) => None,
        })
        .collect::<Vec<_>>();
    if sent.len() != 1 {
        return Err("m7: expected one sent transcript".into());
    }
    let expected = if negative { DEADLINE } else { SUCCESS };
    equal("spoken transcript", &sent[0]["transcript"], &json!(expected))?;
    let frames = call
        .sent
        .iter()
        .filter(|sent| matches!(sent, DialogueSent::Audio(_)))
        .count();
    equal("reply frames", &json!(frames), &json!(10))?;
    Ok(expected.into())
}

#[cfg(test)]
mod tests;
