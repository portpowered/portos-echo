//! Persistent-session oracles over the production trace, forwarded wire
//! receipts and the provider's view of the pinned engine's tool outputs.
use super::Journey;
use serde_json::{Value, json};

/// Listen window configured for every session journey.
const LISTEN_MS: u64 = 3000;
/// One clock step: an expiry is observed on the first step at or past it.
const STEP_MS: u64 = 20;

pub(super) struct Evidence<'a> {
    pub records: &'a [Value],
    /// Wire observations after each reply, then the final one at close.
    pub wire: &'a [Value],
    pub models: &'a [Value],
    pub turns: usize,
}

pub(super) fn check(journey: Journey, evidence: &Evidence) -> Result<Value, String> {
    let summary = common(evidence)?;
    let detail = match journey {
        Journey::Turns => turns(evidence, &summary)?,
        Journey::Idle => idle(&summary)?,
        Journey::Dispatch => dispatch(evidence, &summary)?,
        Journey::Extend => extend(evidence, &summary, 12_000)?,
        Journey::LongWait => long_wait(evidence, &summary)?,
        Journey::End => end(evidence, &summary)?,
        Journey::Limits => limits(evidence, &summary)?,
    };
    Ok(json!({"timeline":summary,"journey":detail}))
}

fn limits(evidence: &Evidence, summary: &Value) -> Result<Value, String> {
    let count = super::LONG_CALLS;
    let results = outputs(evidence, count)?;
    let receipts = receipts(evidence.records, "extend_session");
    if receipts.len() != count {
        return Err(format!(
            "session: expected {count} applied extensions, got {receipts:?}"
        ));
    }
    let mut previous = None;
    for (receipt, output) in receipts.iter().zip(&results) {
        let deadline = receipt["deadline_ms"]
            .as_u64()
            .ok_or("session: extension omitted its deadline")?;
        if receipt["outcome"] != "applied"
            || *output != json!({"deadline_ms":deadline,"capped":false})
            || previous.is_some_and(|prior| deadline != prior + 1000)
        {
            return Err(format!(
                "session: extension effect and tool result disagree: {receipt} {output}"
            ));
        }
        previous = Some(deadline);
    }
    let opened = summary["run_opened_ms"].as_u64().ok_or("session: missing run open")?;
    let elapsed = last_played(summary)?
        .checked_sub(opened)
        .ok_or("session: invalid elapsed time")?;
    if elapsed < super::LONG_WAIT_MS {
        return Err(format!("session: long answer played after only {elapsed} ms"));
    }
    Ok(json!({"successful_calls":count,"elapsed_to_played_ms":elapsed,"turn":dispatch(evidence, summary)?}))
}

fn elapsed(records: &[Value], event: &str) -> Vec<u64> {
    records
        .iter()
        .filter(|r| r["event"] == event)
        .filter_map(|r| r["elapsed_ms"].as_u64())
        .collect()
}

/// One run, one explicit end, every reply audible and Played, no error.
fn common(evidence: &Evidence) -> Result<Value, String> {
    let records = evidence.records;
    let opened = elapsed(records, "run.started");
    let ended = elapsed(records, "wyoming.run_ended");
    let played = elapsed(records, "reply.played");
    if opened.len() != 1 || ended.len() != 1 || elapsed(records, "bridge.run_completed").len() > 1 {
        return Err(format!(
            "session: expected one run and one end, observed opened={opened:?} ended={ended:?}"
        ));
    }
    if played.len() != evidence.turns
        || records
            .iter()
            .any(|r| r["event"] == "reply.played" && r["outcome"] != "played")
    {
        return Err(format!(
            "session: expected {} Played replies, observed {played:?}",
            evidence.turns
        ));
    }
    if records.iter().any(|r| r["event"] == "bridge.error") {
        return Err("session: bridge.error recorded".into());
    }
    let wire = evidence.wire.last().ok_or("session: missing wire observation")?;
    let streams = wire_replies(wire, evidence.turns)?;
    Ok(
        json!({"run_opened_ms":opened[0],"reply_played_ms":played,"run_ended_ms":ended[0],
        "wire_run_pipeline":1,"wire_explicit_end":1,"wire_played":streams,
        "capture_chunks_after_reply_played":wire["capture_chunks_after_reply_played"],"bridge_errors":0}),
    )
}

/// Forwarded device receipts: one run-pipeline, one explicit end, and each
/// reply a complete audible stream that the device reported Played.
fn wire_replies(wire: &Value, turns: usize) -> Result<usize, String> {
    let streams: Vec<_> = wire["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["earcon"] == false)
        .collect();
    let complete = |s: &&Value| {
        s["ending"] == "stop"
            && !s["played_us"].is_null()
            // A later reply also carries the decoder's retained tail frame.
            && s["pcm_bytes"].as_u64().is_some_and(|n| n >= 9600 && n % 960 == 0)
            && s["audible_samples"].as_u64().is_some_and(|n| n > 0)
    };
    if wire["run_opened_us"].as_array().map(Vec::len) != Some(1)
        || wire["explicit_end_us"].as_array().map(Vec::len) != Some(1)
        || streams.len() != turns
        || !streams.iter().all(complete)
    {
        return Err(format!("session: wire receipts mismatch: {wire}"));
    }
    Ok(streams.len())
}

fn last_played(summary: &Value) -> Result<u64, String> {
    summary["reply_played_ms"]
        .as_array()
        .and_then(|values| values.last())
        .and_then(Value::as_u64)
        .ok_or_else(|| "session: missing Played".into())
}

fn ended(summary: &Value) -> Result<u64, String> {
    summary["run_ended_ms"]
        .as_u64()
        .ok_or_else(|| "session: missing end".into())
}

/// The run ends on the first clock step at or after `deadline`.
fn at(deadline: u64, actual: u64, phase: &str) -> Result<(), String> {
    if actual < deadline || actual >= deadline + STEP_MS {
        return Err(format!(
            "session {phase}: expected end at {deadline} ms, observed {actual} ms"
        ));
    }
    Ok(())
}

fn idle(summary: &Value) -> Result<Value, String> {
    let played = last_played(summary)?;
    let end = ended(summary)?;
    at(played + LISTEN_MS, end, "idle")?;
    Ok(json!({"idle_after_played_ms":end - played}))
}

/// Speech heard while the dispatch ran never cancelled it: no interrupt was
/// requested, its answer played, and the run then idled out as usual.
fn dispatch(evidence: &Evidence, summary: &Value) -> Result<Value, String> {
    let interrupts = evidence
        .records
        .iter()
        .filter(|r| {
            matches!(
                r["event"].as_str(),
                Some("turn/interrupt" | "turn.interrupt_acknowledged" | "turn.interrupted")
            )
        })
        .count();
    if interrupts != 0 {
        return Err(format!(
            "session dispatch: user speech interrupted the delegated turn ({interrupts} records)"
        ));
    }
    Ok(json!({"turn_interrupts":0,"idle":idle(summary)?}))
}

fn turns(evidence: &Evidence, summary: &Value) -> Result<Value, String> {
    // The second reply settled before idle expiry, without a second run or end.
    let second = evidence.wire.get(1).ok_or("session: missing second reply wire")?;
    if second["explicit_end_us"].as_array().is_none_or(|ends| !ends.is_empty()) {
        return Err("session: run ended before the second reply's idle expiry".into());
    }
    if summary["capture_chunks_after_reply_played"]
        .as_u64()
        .is_none_or(|n| n == 0)
    {
        return Err("session: capture stopped after the first reply".into());
    }
    idle(summary)
}

fn outputs(evidence: &Evidence, count: usize) -> Result<Vec<Value>, String> {
    let models: Vec<_> = evidence.models.iter().filter(|m| m["turn"] == 0).collect();
    let last = models.last().ok_or("session: missing model request")?;
    let outputs = last["outputs"].as_array().ok_or("session: missing tool outputs")?;
    if models.len() != count + 1 || outputs.len() != count {
        return Err(format!("session: expected {count} tool outputs, observed {models:?}"));
    }
    outputs
        .iter()
        .map(|output| decode(output).ok_or_else(|| format!("session: undecodable tool output {output}")))
        .collect()
}

/// The pinned engine carries a dynamic tool's content as JSON text, either
/// the object itself or one text item holding it.
fn decode(output: &Value) -> Option<Value> {
    let parsed: Value = serde_json::from_str(output.as_str()?).ok()?;
    match parsed.as_array() {
        Some(items) if items.len() == 1 && items[0]["type"] == "text" => {
            serde_json::from_str(items[0]["text"].as_str()?).ok()
        }
        Some(_) => None,
        None => Some(parsed),
    }
}

fn receipts(records: &[Value], tool: &str) -> Vec<Value> {
    records
        .iter()
        .filter(|r| r["event"] == "session.tool_receipt" && r["tool"] == tool)
        .map(|r| json!({"outcome":r["outcome"],"deadline_ms":r["deadline_ms"],"capped":r["capped"],"elapsed_ms":r["elapsed_ms"]}))
        .collect()
}

fn extend(evidence: &Evidence, summary: &Value, maximum: u64) -> Result<Value, String> {
    let expected = json!({"deadline_ms":maximum,"capped":true});
    let results = outputs(evidence, 2)?;
    if results.iter().any(|result| *result != expected) {
        return Err(format!("session: extend results {results:?}"));
    }
    let receipts = receipts(evidence.records, "extend_session");
    if receipts.len() != 2
        || receipts
            .iter()
            .any(|r| r["outcome"] != "applied" || r["deadline_ms"] != maximum || r["capped"] != true)
    {
        return Err(format!("session: extend receipts {receipts:?}"));
    }
    // The cap counts from the controller's admission of the run, after the
    // wake opened it and before the agent's first grant. It ends exactly there.
    let opened = summary["run_opened_ms"].as_u64().ok_or("session: missing run open")?;
    let granted = receipts[0]["elapsed_ms"]
        .as_u64()
        .ok_or("session: missing receipt time")?;
    let end = ended(summary)?;
    let admitted = end.checked_sub(maximum).filter(|at| (opened..=granted).contains(at));
    let admitted = admitted.ok_or_else(|| format!("session: cap end {end} ms outside run {opened}..{granted} ms"))?;
    let played = last_played(summary)?;
    if end <= played + LISTEN_MS {
        return Err("session: extension did not outlive the default listen window".into());
    }
    Ok(
        json!({"results":results,"receipts":receipts,"controller_admitted_ms":admitted,
        "default_deadline_ms":played + LISTEN_MS}),
    )
}

fn long_wait(evidence: &Evidence, summary: &Value) -> Result<Value, String> {
    let mut detail = extend(evidence, summary, 60_000)?;
    let checkpoint = evidence.wire.get(1).ok_or("session: missing long-wait checkpoint")?;
    let epoch = checkpoint["checkpoint_epoch_ms"]
        .as_u64()
        .ok_or("session: missing checkpoint time")?;
    let played = last_played(summary)?;
    if epoch <= played + 30_000 || epoch >= ended(summary)? || checkpoint["explicit_end_us"] != json!([]) {
        return Err(format!(
            "session: premature end or invalid long-wait checkpoint {checkpoint}"
        ));
    }
    // Both native-host connections must receive exactly one configured
    // registration. The 60 s run cap plus 5 s playback allowance is 65 s.
    let expected = json!([[{"hassmic_failsafe_seconds":65}], [{"hassmic_failsafe_seconds":65}]]);
    let wire = evidence.wire.last().ok_or("session: missing final wire")?;
    if wire["registrations"] != expected {
        return Err(format!("session: reconnect watchdog/registration mismatch {wire}"));
    }
    detail["open_checkpoint_ms"] = json!(epoch);
    detail["registrations"] = expected;
    Ok(detail)
}

fn end(evidence: &Evidence, summary: &Value) -> Result<Value, String> {
    let results = outputs(evidence, 1)?;
    if results != [json!({"ending_after_reply":true})] {
        return Err(format!("session: end results {results:?}"));
    }
    let receipts = receipts(evidence.records, "end_session");
    if receipts.len() != 1 || receipts[0]["outcome"] != "applied" {
        return Err(format!("session: end receipts {receipts:?}"));
    }
    let played = last_played(summary)?;
    let end = ended(summary)?;
    // Not before the reply settles, and without waiting for idle expiry.
    if end < played || end >= played + LISTEN_MS {
        return Err(format!("session: end at {end} ms for Played at {played} ms"));
    }
    Ok(json!({"results":results,"receipts":receipts,"end_after_played_ms":end - played}))
}
