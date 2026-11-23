//! T1: the delegated speed tier on `thread/start`, and the trace of a silent downgrade.
use super::*;
use echo_config::DelegateSpeed;

/// Start one call at `speed`, answer `thread/start` with `result`, and return
/// the runner request, the Live request and the trace rows.
async fn start_at(speed: DelegateSpeed, result: Value) -> (Value, Value, Vec<Value>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
    let (mut io, mut server) = fixture();
    let client = async {
        let options = Options {
            speed,
            effort: Some("low"),
            ..options()
        };
        let mut call = Call::new(&mut io);
        call.setup_trace = Some(trace);
        call.start(&options, vec![], "fixture-offer", deadline()).await.unwrap()
    };
    let peer = async {
        let runner = read(&mut server).await;
        assert_eq!(runner["method"], "thread/start");
        write(&mut server, json!({"id":runner["id"],"result":result})).await;
        let live = read(&mut server).await;
        write(&mut server, json!({"id":live["id"],"result":{}})).await;
        notification(
            &mut server,
            "thread/realtime/sdp",
            json!({"threadId":"fixture-thread","sdp":"fixture-answer"}),
        )
        .await;
        (runner, live)
    };
    let (answer, (runner, live)) = tokio::join!(client, peer);
    assert_eq!(answer, "fixture-answer");
    let rows = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (runner, live, rows)
}

fn events(rows: &[Value]) -> Vec<&str> {
    rows.iter().map(|row| row["event"].as_str().unwrap()).collect()
}

fn thread_result(tier: Option<&str>) -> Value {
    let mut result = json!({"thread":{"id":"fixture-thread"}});
    if let Some(tier) = tier {
        result["serviceTier"] = tier.into();
    }
    result
}

#[tokio::test(start_paused = true)]
async fn runner_speed_sends_exact_tier_and_does_not_change_live_request() {
    let (mut standard, live_standard, _) = start_at(DelegateSpeed::Standard, thread_result(None)).await;
    let standard_tier = standard["params"].as_object_mut().unwrap().remove("serviceTier");
    assert_eq!(standard_tier, Some(Value::Null));
    for (speed, tier) in [
        (DelegateSpeed::Fast, "priority"),
        (DelegateSpeed::Ultrafast, "ultrafast"),
    ] {
        let (mut runner, live, rows) = start_at(speed, thread_result(Some(tier))).await;
        assert_eq!(
            runner["params"].as_object_mut().unwrap().remove("serviceTier"),
            Some(json!(tier))
        );
        assert_eq!(runner, standard);
        assert_eq!(live, live_standard);
        assert_eq!(live["method"], "thread/realtime/start");
        // The effective tier equals the request: no downgrade row.
        assert_eq!(events(&rows), ["call.realtime_start_sent", "call.answer_received"]);
    }
}

#[tokio::test(start_paused = true)]
async fn downgraded_tier_is_traced_without_failing_the_call() {
    for (speed, requested, effective) in [
        (DelegateSpeed::Ultrafast, "ultrafast", json!("default")),
        (DelegateSpeed::Fast, "priority", json!("default")),
        (DelegateSpeed::Ultrafast, "ultrafast", json!("priority")),
        (DelegateSpeed::Ultrafast, "ultrafast", json!(null)),
    ] {
        let tier = effective.as_str();
        let (_, _, rows) = start_at(speed, thread_result(tier)).await;
        if tier.is_none() {
            // An absent or null effective tier reports nothing.
            assert_eq!(events(&rows), ["call.realtime_start_sent", "call.answer_received"]);
            continue;
        }
        assert_eq!(
            events(&rows),
            [
                "call.service_tier_downgraded",
                "call.realtime_start_sent",
                "call.answer_received"
            ]
        );
        assert_eq!(rows[0]["requested"], requested);
        assert_eq!(rows[0]["effective"], effective);
    }
    // A standard request is never reported, whatever the server answers.
    let (_, _, rows) = start_at(DelegateSpeed::Standard, thread_result(Some("default"))).await;
    assert_eq!(events(&rows), ["call.realtime_start_sent", "call.answer_received"]);
    // A value that is not a tier name is not copied into the trace.
    let (_, _, rows) = start_at(DelegateSpeed::Fast, thread_result(Some("Not A Tier!"))).await;
    assert_eq!(rows[0]["event"], "call.service_tier_downgraded");
    assert_eq!(rows[0]["effective"], Value::Null);
}
