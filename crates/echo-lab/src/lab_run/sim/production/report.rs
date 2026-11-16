//! Aggregate only observed measurements; absent data never becomes a zero.
use super::super::{selectors, world::World};
use serde_json::{Value, json};

pub fn collect(world: &World) -> Value {
    let mut wakes = Vec::new();
    let mut windows = Vec::new();
    for generation in 1..=world.generation {
        for record in world.records(generation) {
            if record["event"] == "wyoming.mic_frames" {
                wakes.push(
                    json!({"bridge_generation":generation,"run_id":record["run_id"],"mic_frames":record["count"]}),
                );
            }
            if record["event"] == "mic.mute_window" {
                windows.push(record);
            }
        }
    }
    let mut result = json!({"ready_before_first_wake":world.initial_ready,
        "streamed_wakes":wakes,
        "bridge_restarts":world.restarts.iter().filter(|r| r["process"] == "bridge").count(),
        "hassmic_host_restarts":world.restarts.iter().filter(|r| r["process"] == "hassmic-host").count(),
        "mute_status":"fail"});
    if let [window] = windows.as_slice() {
        result["mute_window"] =
            json!({"received_frames":window["received_frames"],"forwarded_frames":window["forwarded_frames"]});
        if window["received_frames"] == 0 && window["forwarded_frames"] == 0 {
            result["mute_status"] = json!("pass");
        }
    }
    result
}

pub fn check(world: &World, measured: &Value) -> Result<(), String> {
    if measured["ready_before_first_wake"] != 1
        || measured["bridge_restarts"] != 10
        || measured["hassmic_host_restarts"] != 10
        || measured["mute_status"] != "pass"
    {
        return Err("M1: expected Ready=1, mute received=0/forwarded=0, bridge restarts=10, host restarts=10".into());
    }
    let wakes = measured["streamed_wakes"]
        .as_array()
        .ok_or("M1: missing streamed wakes")?;
    if wakes.len() != 22 {
        return Err(format!("M1: expected 22 streamed wakes, observed {}", wakes.len()));
    }
    let oracle = selectors::parse(&[
        json!("wyoming.run_started"),
        json!("wyoming.mic_frames {count_gte: 10}"),
        json!("wyoming.run_ended"),
    ])?;
    let mut seen = std::collections::BTreeSet::new();
    for wake in wakes {
        let generation = wake["bridge_generation"].as_u64().ok_or("M1: invalid generation")?;
        let run = wake["run_id"].as_str().ok_or("M1: missing run ID")?;
        if !seen.insert((generation, run)) {
            return Err("M1: duplicate generation/run measurement".into());
        }
        selectors::check(world.records(generation), &oracle, Some(run))?;
    }
    let mut recovered = std::collections::BTreeSet::new();
    for restart in &world.restarts {
        let generation = restart["bridge_generation"]
            .as_u64()
            .ok_or("M1: missing restart generation")?;
        let run = restart["run_id"].as_str().ok_or("M1: missing restart run ID")?;
        if !seen.contains(&(generation, run)) || !recovered.insert((generation, run)) {
            return Err("M1: restart must identify its own complete streamed wake".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::super::trace::Trace;
    use super::*;

    fn observed() -> World {
        let mut world = World::default();
        world.generation = 11;
        world.initial_ready = Some(1);
        for generation in 1..=11 {
            let mut trace = Trace::default();
            let count = if generation == 1 {
                2
            } else if generation == 11 {
                11
            } else {
                1
            };
            for run in 1..=count {
                let id = format!("run-{run}");
                for (event, elapsed) in [
                    ("wyoming.run_started", 1),
                    ("wyoming.mic_frames", 2),
                    ("wyoming.run_ended", 3),
                ] {
                    let mut record = json!({"event":event,"run_id":id,"elapsed_ms":elapsed});
                    if event == "wyoming.mic_frames" {
                        record["count"] = json!(10);
                    }
                    trace.append(format!("{record}\n").as_bytes()).unwrap();
                }
                if generation > 1 {
                    world.restarts.push(json!({"process":if run == 1 { "bridge" } else { "hassmic-host" },"bridge_generation":generation,"run_id":id}));
                }
            }
            if generation == 1 {
                trace.append(b"{\"event\":\"mic.mute_window\",\"elapsed_ms\":4,\"window_id\":\"mute-1\",\"received_frames\":0,\"forwarded_frames\":0}\n").unwrap();
            }
            if generation == 11 {
                world.trace = trace;
            } else {
                world.history.push(trace);
            }
        }
        world
    }

    #[test]
    fn aggregate_keeps_reused_run_ids_separate_and_requires_every_restart_wake() {
        let mut world = observed();
        let measured = collect(&world);
        assert_eq!(check(&world, &measured), Ok(()));
        assert_eq!(measured["streamed_wakes"].as_array().unwrap().len(), 22);
        assert_eq!(measured["bridge_restarts"], 10);
        assert_eq!(measured["hassmic_host_restarts"], 10);
        assert_eq!(
            measured["mute_window"],
            json!({"received_frames":0,"forwarded_frames":0})
        );
        assert_eq!(world.records(2)[1]["run_id"], "run-1");
        assert_eq!(world.records(3)[1]["run_id"], "run-1");
        world.trace.records[1]["count"] = json!(9);
        assert!(check(&world, &collect(&world)).unwrap_err().contains("observed 0"));
        world.trace.records[1]["count"] = json!(10);
        world.restarts[1] = world.restarts[0].clone();
        assert!(
            check(&world, &collect(&world))
                .unwrap_err()
                .contains("own complete streamed wake")
        );
    }

    #[test]
    fn aggregate_rejects_missing_mute_counts_and_cross_generation_or_reordered_evidence() {
        let mut world = observed();
        world.history[0]
            .records
            .last_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("received_frames");
        assert_eq!(collect(&world)["mute_status"], "fail");
        assert!(check(&world, &collect(&world)).is_err());
        world.history[0].records.last_mut().unwrap()["received_frames"] = json!(0);
        world.history[2].records.swap(0, 2);
        assert!(check(&world, &collect(&world)).unwrap_err().contains("order"));
        world.history[2].records.clear();
        assert_eq!(collect(&world)["streamed_wakes"].as_array().unwrap().len(), 21);
        assert!(check(&world, &collect(&world)).unwrap_err().contains("observed 21"));
    }
}
