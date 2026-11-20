//! Observe the new sample, the independent question trigger and the wire receipt.
use super::{REPEATED, World, clock::Clock};
use crate::lab_run::run_report::Artifacts;
use serde_json::{Value, json};

const ZONE: &str = " UTC+00:00; operator time zone: America/Los_Angeles; answer time questions in America/Los_Angeles]";
const QUIET: &str =
    "Use this newer time snapshot for time answers. This is quiet context. Do not read it aloud or reply to it.";

pub(super) async fn ask(world: &mut World, clock: &Clock, index: usize) -> Result<(), String> {
    if index == 0 {
        world.satellite.wake().await?;
        world.connected().await?;
    } else {
        world
            .wait("first run ended", |world| world.count("wyoming.run_ended") == 1, 20)
            .await?;
        let samples = corrected_samples(world);
        if samples != 1 {
            return Err(format!(
                "time refresh: expected one initial corrected sample, observed {samples}"
            ));
        }
        clock.select_later_sample();
        // Let the local peer answer ICE keepalives as virtual time advances.
        for _ in 0..60 {
            if corrected_samples(world) > samples {
                break;
            }
            world.provider.tick().await?;
            world.step(1_000).await?;
        }
        world
            .wait(
                "new corrected SNTP sample",
                |world| corrected_samples(world) > samples,
                0,
            )
            .await?;
        world.run_id = "run-2";
    }
    world.provider.arm_time_question(REPEATED[index]).await?;
    world.satellite.speak(index != 0).await
}

fn corrected_samples(world: &World) -> usize {
    world
        .trace
        .records
        .iter()
        .filter(|row| row["event"] == "clock.offset" && row["synced"] == true && row["corrected"] == true)
        .count()
}

pub(super) fn answer(result: &Value, index: usize) -> Result<&str, String> {
    let (source, minute, order) = if index == 0 {
        ("Call-start", "30", 1)
    } else {
        ("Wake-time", "37", 3)
    };
    let seconds = result["context"]
        .as_str()
        .and_then(|line| line.strip_prefix(&format!("[{source} context: 2026-02-03 Tuesday 03:{minute}:")))
        .and_then(|rest| rest.strip_suffix(ZONE));
    let expected = format!("19:{minute}");
    if result["question"] != REPEATED[index]
        || result["answer"] != expected
        || result["trigger"] != "uplink_audio"
        || result["order"] != order
        || !seconds.is_some_and(|part| part.len() == 2 && part.parse::<u8>().is_ok_and(|n| (5..60).contains(&n)))
    {
        return Err(format!(
            "time refresh: expected {expected} from {source} after uplink, observed {result}"
        ));
    }
    result["answer"]
        .as_str()
        .ok_or_else(|| "time refresh: missing direct transcript".into())
}

pub(super) fn check(
    snapshot: &Value,
    records: &[Value],
    counts: &Value,
    turns: usize,
    artifacts: &mut Artifacts,
) -> Result<(), String> {
    let wire = &snapshot["time_wire"];
    let decisions = &snapshot["media_time_turns"];
    let bridge = |event, run: Option<&str>| {
        records
            .iter()
            .filter(|row| row["event"] == event && run.is_none_or(|id| row["run_id"] == id))
            .count()
    };
    let runs = ["run-1", "run-2"].map(|id| {
        json!({"id":id,"started":bridge("wyoming.run_started", Some(id)),"played":bridge("reply.played", Some(id))})
    });
    let evidence = json!({"wire":wire,"decisions":decisions,
        "bridge_peer_created":bridge("call.peer_created", None),"bridge_realtime_start_sent":bridge("call.realtime_start_sent", None),
        "runs":runs,
    });
    artifacts.event("session.live_time_refresh", None, Some(&evidence.to_string()))?;
    // Preserve evidence on a partial run; the failed step reports its own cause.
    if turns != REPEATED.len() {
        return Ok(());
    }
    let context = decisions[1]["context"]
        .as_str()
        .ok_or("time refresh: missing selected wake context")?;
    let expected = json!([{"order":2,"item":{"type":"session.context.append", "content":[
        {"type":"input_text", "text":format!("{context}\n{QUIET}")}
    ]}}]);
    if *wire != expected
        || decisions.as_array().map(Vec::len) != Some(2)
        || evidence["bridge_peer_created"] != 1
        || evidence["bridge_realtime_start_sent"] != 1
        || evidence["runs"] != json!([{"id":"run-1","started":1,"played":1},{"id":"run-2","started":1,"played":1}])
    {
        return Err(format!(
            "time refresh: expected one quiet item before the second of two independent replies, observed {evidence}"
        ));
    }
    super::oracle::direct_only(counts, 2, 2)
}
