//! Compare the received call snapshot, response, and playback independently.
use super::{CONFIGURED_TURNS, QUESTIONS, World};
use crate::lab_run::run_report::Artifacts;
use serde_json::{Value, json};

pub(super) fn answer(result: &Value, question: usize) -> Result<&str, String> {
    let prefix = "[Call-start context: 2026-02-03 Tuesday 03:";
    let suffix = if question < CONFIGURED_TURNS {
        " UTC+00:00; operator time zone: America/Los_Angeles; answer time questions in America/Los_Angeles]"
    } else {
        " UTC+00:00; local offset unavailable]"
    };
    let context = result["context"]
        .as_str()
        .ok_or("time answers: missing received context")?;
    let (minute, second) = context
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .and_then(|clock| clock.split_once(':'))
        .filter(|(minute, second)| {
            [minute, second]
                .iter()
                .all(|part| part.len() == 2 && part.parse::<u8>().is_ok_and(|n| n < 60))
        })
        .ok_or_else(|| format!("time answers: unexpected received context {context}"))?;
    // The fixture is in February, so 03:mm UTC is 19:mm on the prior day in Los Angeles.
    let expected = match question {
        0 => format!("19:{minute}"),
        1 => "2026-02-02".to_owned(),
        2 => "Monday".to_owned(),
        _ => format!("03:{minute} UTC. I don't know your local time zone."),
    };
    let actual = result["answer"]
        .as_str()
        .ok_or("time answers: missing direct transcript")?;
    if result["question"] != QUESTIONS[question] || actual != expected {
        return Err(format!(
            "time answers: expected {expected:?}, received {result} at 03:{minute}:{second} UTC"
        ));
    }
    Ok(actual)
}

pub(super) fn reply(world: &World, result: &Value, index: usize) -> Result<Value, String> {
    let wire = world.satellite.observed.borrow().session_observation();
    let streams: Vec<_> = wire["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|stream| stream["earcon"] == false)
        .collect();
    let stream = streams.get(index).ok_or("time answers: missing Wyoming reply stream")?;
    if streams.len() != index + 1
        || stream["ending"] != "stop"
        || stream["audible_samples"].as_u64().is_none_or(|samples| samples < 2_400)
        || stream["played_us"].as_u64().is_none()
    {
        return Err(format!(
            "time answers: expected an audible Wyoming reply with Played, observed {stream}"
        ));
    }
    Ok(json!({
        "event":"session.live_time_answer","question":result["question"],"context":result["context"],
        "transcript":result["answer"],"run_id":world.run_id,"reply":index + 1,"wyoming":stream,
    }))
}

pub(super) fn delegated(world: &World, artifacts: &mut Artifacts, result: &Value) -> Result<(), String> {
    let observed = world.provider.observed.borrow();
    let delegations = world.count("delegation.created");
    let id = &observed["delegations"][0];
    if delegations != 1
        || observed["delegations"].as_array().map(Vec::len) != Some(1)
        || observed["requests"][0] != result["question"]
        || !world.trace.records.iter().any(|row| {
            row["event"] == "delegation.created" && row["run_id"] == world.run_id && row["delegation_id"] == *id
        })
    {
        return Err("time answers: unexpected provider or bridge delegation identity".into());
    }
    artifacts.measurement(json!({
        "event":"session.live_time_delegated","question":result["question"],"context":result["context"],
        "provider_delegations":1,"bridge_delegations":delegations,"delegation_id":id,"run_id":world.run_id,
    }))?;
    Err(format!(
        "time answers: expected a direct reply to {}, observed one delegation.created ({id})",
        result["question"]
    ))
}

pub(super) fn counts(snapshot: &Value, records: &[Value], configured: bool) -> Value {
    let count = |event| records.iter().filter(|row| row["event"] == event).count();
    json!({
        "event":"session.live_time_counts","configured":configured,
        "calls":snapshot["calls"],"sidebands":snapshot["sidebands"],"external":snapshot["external"],
        "provider_delegations":snapshot["delegations"].as_array().map(Vec::len),
        "bridge_delegations":count("delegation.created"),"model_requests":snapshot["model_requests"],
        "tool_calls":count("delegation.tool") + count("item/tool/call"),
        "runs":count("wyoming.run_started"),"replies_played":count("reply.played"),
    })
}

pub(super) fn direct_only(evidence: &Value, replies: usize) -> Result<(), String> {
    if evidence["calls"] != 1
        || evidence["sidebands"] != 1
        || evidence["external"] != 0
        || evidence["provider_delegations"] != 0
        || evidence["bridge_delegations"] != 0
        || evidence["model_requests"] != 0
        || evidence["tool_calls"] != 0
        || evidence["runs"] != 1
        || evidence["replies_played"] != replies
    {
        return Err(format!(
            "time answers: expected {replies} direct replies in one local call, observed {evidence}"
        ));
    }
    Ok(())
}
