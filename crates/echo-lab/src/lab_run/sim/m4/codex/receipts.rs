//! Causal join of independent native sideband, bridge dispatch and MCP witnesses.
use crate::lab_run::sim::trace::Trace;
use echo_spoof_mcp::{CallRecord, Snapshot};
use serde_json::{Value, json};

/// Every projected clock receipt must have a matching paired SSE publication.
pub(super) fn screen(frames: &[Value], clock: &Value) -> Result<(), String> {
    let states = clock["states"].as_array().ok_or("m4: missing screen clock receipts")?;
    if frames.len() != states.len() {
        return Err("m4: incomplete paired screen sequence".into());
    }
    let mut previous = 0;
    for (index, (frame, stamp)) in frames.iter().zip(states).enumerate() {
        let epoch = stamp["epoch_ms"].as_u64().ok_or("m4: missing screen timestamp")?;
        if frame["seq"].as_u64() != u64::try_from(index + 1).ok()
            || frame["state"] != stamp["state"]
            || epoch < previous
        {
            return Err("m4: paired screen/clock sequence mismatch".into());
        }
        previous = epoch;
    }
    Ok(())
}

pub(super) fn merge_mode(
    trace: &mut Trace,
    snapshot: &Value,
    mcp: &Snapshot,
    negative: bool,
    emotion: bool,
) -> Result<(), String> {
    let name = if emotion { "ui.show_emotion" } else { "test.echo" };
    let args = if emotion {
        json!({"name":"happy"})
    } else {
        json!({"text":"delegated echo"})
    };
    if snapshot["delegations"] != json!(["delegation-local"]) {
        return Err("m4: missing or duplicate native delegation.created".into());
    }
    if snapshot["appends"] != json!(["delegation-local"]) {
        return Err("m4: missing verified tool result/delegation.context.append for delegation-local".into());
    }
    if snapshot != &expected(negative, emotion) {
        return Err("m4: exact pinned provider/model observation mismatch".into());
    }
    check_mcp(mcp, name, args)?;
    let request = position(trace, "item/tool/call")?;
    let attempt = position(trace, "tools/call")?;
    let transcript = trace
        .records
        .iter()
        .position(|r| r["event"] == "reply.played")
        .ok_or("m4: missing played reply observation")?;
    if request >= attempt || attempt >= transcript {
        return Err("m4: dispatch/attempt/reply causal order mismatch".into());
    }
    let received = &trace.records[request];
    let executed = &trace.records[attempt];
    if received["name"] != name
        || received["call_id"] != "tool-local"
        || received["run_id"] != "run-1"
        || !["thread_id", "turn_id", "call_id", "run_id", "name"]
            .into_iter()
            .all(|key| !received[key].is_null() && received[key] == executed[key])
    {
        return Err("m4: native model/tool/host correlation mismatch".into());
    }
    // Native append is emitted only after the fake verified the returned model
    // output, and before it permits the reply media/transcript. These receipts
    // have causal anchors, not invented wall-clock measurements.
    let native = |event: &str, anchor: &Value| {
        json!({"event":event,"source":"provider.sideband",
        "delegation_id":"delegation-local","run_id":anchor["run_id"],"thread_id":anchor["thread_id"],
        "turn_id":anchor["turn_id"],"call_id":anchor["call_id"],
        "elapsed_ms":anchor["elapsed_ms"],"provider_auth_mode":anchor["provider_auth_mode"]})
    };
    let created = native("delegation.created", received);
    let appended = native("delegation.context.append", executed);
    trace.records.insert(attempt + 1, appended);
    trace.records.insert(request, created);
    Ok(())
}

pub(super) fn expected(negative: bool, emotion: bool) -> Value {
    json!({"event":"provider.snapshot", "calls":[{
        "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
        "model":"gpt-live-1-codex", "alpha":"quicksilver=v2"}], "sidebands":1,"external":0,
        "delegations":["delegation-local"], "appends":["delegation-local"],
        "transcripts":[{"role":"user","text":"ping"},{"role":"assistant","text":if negative {super::super::model::FALLBACK} else if emotion {super::super::emotion::REPLY} else {super::super::model::SUCCESS}}],
        "models":[{"tools":["test_echo","test_wait","test_requires_approval","ui_show_emotion"],"outputs":0},
                  {"tools":["test_echo","test_wait","test_requires_approval","ui_show_emotion"],"outputs":1}]})
}

fn position(trace: &Trace, event: &str) -> Result<usize, String> {
    let positions: Vec<_> = trace
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| r["event"] == event)
        .map(|(index, _)| index)
        .collect();
    match positions.as_slice() {
        [index] => Ok(*index),
        _ => Err(format!("m4: expected exactly one {event} receipt")),
    }
}

fn check_mcp(mcp: &Snapshot, name: &str, args: Value) -> Result<(), String> {
    if mcp.calls
        != vec![CallRecord {
            name: name.into(),
            args,
            count: 1,
        }]
        || mcp.requests.iter().filter(|r| r.method == "tools/call").count() != 1
    {
        return Err("m4: expected exactly one observed MCP test.echo attempt".into());
    }
    Ok(())
}
