//! Join the bridge's run trace, the Wyoming wire and pinned runner results.
use crate::lab_run::sim::m4::wire::Timing;
use serde_json::{Value, json};

pub(super) fn farewell(
    records: &[Value],
    wire: &Timing,
    provider: &Value,
    (run, turn, reply, late): (&str, usize, usize, bool),
    endpoint: u64,
    clock: &Value,
) -> Result<Value, String> {
    let played = rows(records, run, "reply.played");
    let end = one(records, run, "wyoming.run_ended")?;
    let receipt = one(records, run, "session.tool_receipt")?;
    let tool = one(records, run, "item/tool/call")?;
    // A hand-off after Played opens one more reply like any other hand-off.
    // The runner's silent end closes it before any audio: it must stay empty.
    let expected = if late || (run == "run-1" && reply == 1) { 2 } else { 1 };
    let spoken = played.len().checked_sub(1 + usize::from(late));
    let last = spoken
        .and_then(|index| played.get(index))
        .ok_or("goodbye: missing bridge Played")?;
    let farewell_at = time(last)?;
    let end_at = time(end)?;
    if end_at < farewell_at
        // No ordinary listen window ran between the farewell and the end.
        || end_at - farewell_at >= 3_000
        || (late && played.last().is_some_and(|empty| empty["received_frames"] != 0 || empty["frames"] != 0))
        || end_at < time(receipt)?
        || end_at < endpoint
        || (late && time(one(records, run, "delegation.created")?)? <= farewell_at)
        || (late && time(receipt)? <= farewell_at)
        || (!late && time(receipt)? > farewell_at)
        || receipt["tool"] != "end_session"
        || receipt["outcome"] != "applied"
        || tool["name"] != "end_session"
    {
        return Err("goodbye: incorrect receipt, tool or playback ordering".into());
    }
    if played.len() != expected || played.iter().any(|row| row["outcome"] != "played") {
        return Err("goodbye: extra, missing or cut bridge reply".into());
    }
    // An applied end interrupts the runner's turn when it is still running
    // (decision 2026-11-21): that is the end, not a failure.
    if records
        .iter()
        .any(|row| matches!(row["event"].as_str(), Some("bridge.error" | "live.closed")))
    {
        return Err("goodbye: bridge failure or premature Live call close".into());
    }
    handback(records, run, turn, provider)?;
    let ends = if run == "run-2" { 2 } else { 1 };
    let frames = wire_replies(wire, clock, (reply + 1, late), ends)?;
    Ok(json!({
        "event":"session.live_goodbye","run_id":run,"handoff":super::goodbye::GOODBYE,
        "runner_tool_answer":{"ending_after_reply":true},"applied_receipts":1,
        "tool_after_played":late,"endpoint_epoch_ms":endpoint,"run_end_epoch_ms":end_at,
        "goodbye_to_run_end_ms":end_at - endpoint,"reply_played_ms":farewell_at,
        "farewell_to_run_end_ms":end_at - farewell_at,"pcm_after_farewell_bytes":0,
        "played_replies":expected,"farewell_frames":frames[reply],"wire_ends_for_run":1,
        "post_reply_admissions":0,"screen":"ready"
    }))
}

fn handback(records: &[Value], run: &str, turn: usize, provider: &Value) -> Result<(), String> {
    let id = format!("session-turn-{turn}");
    let created: Vec<_> = rows(records, run, "delegation.created")
        .into_iter()
        .filter(|row| row["delegation_id"] == id)
        .collect();
    let models: Vec<_> = provider["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|model| model["turn"] == turn)
        .collect();
    // The runner sees its tool answer unless the end's interrupt stopped the
    // turn before its next model request.
    let interrupted = records.iter().any(|row| row["event"] == "turn.interrupted");
    let answered = models.len() == 2
        && models[1]["outputs"].as_array().map(Vec::len) == Some(1)
        && tool_result(&models[1]["outputs"][0]) == Some(json!({"ending_after_reply": true}));
    if created.len() != 1
        || provider["requests"][turn] != super::goodbye::GOODBYE
        || !real_handoff(&created[0]["task"])
        || models.first().is_none_or(|model| model["outputs"] != json!([]))
        || !(answered || (interrupted && models.len() == 1))
        || provider["appends"]
            .as_array()
            .is_none_or(|appends| appends.contains(&json!(id)))
    {
        return Err("goodbye: expected one fixed hand-off, one tool answer and a silent runner".into());
    }
    Ok(())
}

/// The pinned app server's hand-off: the user's words and the transcript
/// since the last hand-off. Nothing in it is written by the voice model.
fn real_handoff(task: &Value) -> bool {
    let words = super::goodbye::GOODBYE;
    task.as_object().is_some_and(|fields| fields.len() == 5)
        && task["type"] == "handoff_request"
        && task["handoff_id"].is_string()
        && task["item_id"].is_string()
        && task["input_transcript"] == words
        && task["active_transcript"]
            .as_array()
            .is_some_and(|entries| entries.contains(&json!({"role":"user","text":words})))
}

fn tool_result(output: &Value) -> Option<Value> {
    let parsed: Value = serde_json::from_str(output.as_str()?).ok()?;
    match parsed.as_array() {
        Some(items) if items.len() == 1 && items[0]["type"] == "text" => {
            serde_json::from_str(items[0]["text"].as_str()?).ok()
        }
        Some(_) => None,
        None => Some(parsed),
    }
}

pub(super) fn question(records: &[Value], wire: &Timing, clock: &Value) -> Result<Value, String> {
    let played = rows(records, "run-1", "reply.played");
    let ended = time(one(records, "run-1", "wyoming.run_ended")?)?;
    if played.len() != 2
        || played.iter().any(|row| row["outcome"] != "played")
        || ended.saturating_sub(time(played[1])?) < 3_000
        || !rows(records, "run-1", "session.tool_receipt").is_empty()
        || !rows(records, "run-1", "item/tool/call").is_empty()
        || records.iter().any(|row| row["event"] == "bridge.error")
    {
        return Err("goodbye: question did not retain an uninterrupted run through ordinary idle".into());
    }
    let frames = wire_replies(wire, clock, (2, false), 1)?;
    Ok(
        json!({"event":"session.live_thanks_question","run_id":"run-1","played_replies":2,
        "reply_frames":frames,"early_ends":0,"end_tool_calls":0,"end_receipts":0,
        "idle_after_played_ms":ended - time(played[1])?,"screen":"ready"}),
    )
}

/// Check `spoken` audible replies, then one empty stream when `empty_tail`.
fn wire_replies(
    wire: &Timing,
    clock: &Value,
    (spoken, empty_tail): (usize, bool),
    ends: usize,
) -> Result<Vec<usize>, String> {
    let replies = spoken + usize::from(empty_tail);
    let state = wire.session_observation();
    let streams: Vec<_> = state["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|stream| stream["earcon"] == false)
        .collect();
    let counts = wire.reply_frames();
    let ended = state["explicit_end_us"]
        .as_array()
        .ok_or("goodbye: missing wire ends")?;
    if streams.len() != replies
        || counts.len() != replies
        || ended.len() != ends
        || state["run_opened_us"].as_array().map(Vec::len) != Some(ends)
        || state["unmatched_played"] != 0
    {
        return Err(format!("goodbye: incorrect wire run or reply counts: {state}"));
    }
    let mut last_played = 0;
    let mut frame_counts = vec![];
    for (index, (stream, frames)) in streams.iter().zip(&counts).enumerate() {
        let played = stream["played_us"].as_u64().ok_or("goodbye: missing device Played")?;
        if index == spoken {
            if stream["ending"] != "stop" || stream["pcm_bytes"] != 0 || !frames.is_empty() || played < last_played {
                return Err(format!("goodbye: audio followed the farewell: {stream}"));
            }
            last_played = played;
            continue;
        }
        if stream["ending"] != "stop"
            || played < last_played
            // The first reply loses the decoder warm-up. A subsequent
            // reply can also contain the retained decoder tail.
            || !(10..=if index == 0 { 10 } else { 11 }).contains(&frames.len())
            || frames.iter().any(|(bytes, _, _)| *bytes != 960)
            || stream["audible_samples"].as_u64().is_none_or(|samples| samples < 2_400)
        {
            return Err(format!(
                "goodbye: incorrect wire audio sequence: {stream}, frames={frames:?}"
            ));
        }
        last_played = played;
        frame_counts.push(frames.len());
    }
    let last_end = ended
        .last()
        .and_then(Value::as_u64)
        .ok_or("goodbye: missing final wire end")?;
    if last_end < last_played {
        return Err("goodbye: Wyoming ended before farewell Played".into());
    }
    wire_matches_bridge(clock, counts, frame_counts.iter().sum())?;
    Ok(frame_counts)
}

/// Every PCM frame the bridge submitted is the frame the device received.
fn wire_matches_bridge(clock: &Value, counts: Vec<&[(usize, usize, usize)]>, total: usize) -> Result<(), String> {
    let submitted = clock["pcm"].as_array().ok_or("goodbye: missing bridge PCM receipts")?;
    let observed = counts.into_iter().flatten();
    if submitted.len() != total
        || submitted
            .iter()
            .zip(observed)
            .any(|(frame, &(bytes, nonzero, audible))| {
                frame["pcm_bytes"] != bytes
                    || frame["nonzero_samples"] != nonzero
                    || frame["audible_samples"] != audible
            })
    {
        return Err("goodbye: wire and bridge disagree on the farewell frame sequence".into());
    }
    Ok(())
}

fn rows<'a>(records: &'a [Value], run: &str, event: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|row| row["run_id"] == run && row["event"] == event)
        .collect()
}

fn one<'a>(records: &'a [Value], run: &str, event: &str) -> Result<&'a Value, String> {
    match rows(records, run, event).as_slice() {
        [record] => Ok(*record),
        _ => Err(format!("goodbye: expected exactly one {event} for {run}")),
    }
}

fn time(record: &Value) -> Result<u64, String> {
    record["elapsed_ms"]
        .as_u64()
        .ok_or_else(|| "goodbye: missing run clock timestamp".into())
}
