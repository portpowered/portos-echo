use super::{
    journey::*,
    model::{Model, Reply, final_item},
};
use crate::{config, sse};
use echo_config::LiveMode;

#[tokio::test]
async fn d03_d04_d05_configured_call_boundaries() {
    for (cap, requested, finish, expected) in [
        (8, 9, false, "tool call limit reached"),
        (8, 8, true, "eight complete"),
        (1, 2, false, "tool call limit reached"),
    ] {
        let texts: Vec<_> = (0..requested).map(|i| format!("echo-{i}")).collect();
        let mut replies: Vec<_> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| Reply::body(sse(&[function(i, t)])))
            .collect();
        if finish {
            replies.push(Reply::body(sse(&[final_item(expected)])));
        }
        let model = Model::start(replies).await;
        let mut cfg = config();
        cfg.max_tool_calls = cap;
        let (mut runner, spoof, sink) = setup(cfg, &model, LiveMode::ApiKey, true).await;
        assert_eq!(
            runner.run(event("d-cap"), "i".into()).await.unwrap(),
            Some(commentary("d-cap", expected))
        );
        let count = usize::try_from(cap).unwrap();
        assert_eq!(
            spoof.snapshot().unwrap().calls,
            calls(&texts[..count].iter().map(String::as_str).collect::<Vec<_>>())
        );
        trace(&sink, "d-cap", count);
        record(&format!(
            "D03/D04/D05 configured_cap={cap} requested={requested} executed={count} commentary={expected:?}"
        ));
        assert_eq!(model.finish().await.len(), requested + usize::from(finish));
        drop(runner);
        spoof.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn d08_budget_is_reused() {
    let model = Model::start(vec![Reply::body(sse(&[final_item("direct answer")]))]).await;
    let mut cfg = config();
    cfg.daily_request_cap = 1;
    let (mut runner, spoof, sink) = setup(cfg, &model, LiveMode::ApiKey, true).await;
    for (id, text) in [("d-one", "direct answer"), ("d-two", "daily request limit reached")] {
        assert_eq!(
            runner.run(event(id), "i".into()).await.unwrap(),
            Some(commentary(id, text))
        );
    }
    assert_eq!(model.finish().await.len(), 1);
    assert_eq!(spoof.snapshot().unwrap().calls, vec![]);
    record("D08/D11 exact request count=1 tools=0 terminal verified");
    assert_eq!(
        sink.0
            .lock()
            .unwrap()
            .iter()
            .map(echo_delegation::TraceEvent::name)
            .collect::<Vec<_>>(),
        vec![
            "session.delegation.created",
            "session.commentary.append",
            "session.delegation.created",
            "session.commentary.append"
        ]
    );
    drop(runner);
    spoof.shutdown().await.unwrap();
}
#[tokio::test]
async fn d09_sequential_contexts_utf8_and_reused_call_id() {
    let model = Model::start(vec![
        Reply::body(sse(&[function(0, "雪")])),
        Reply::body(sse(&[final_item("雪☃")])),
        Reply::body(sse(&[function(0, "é")])),
        Reply::body(sse(&[final_item("é✓")])),
    ])
    .await;
    let (mut runner, spoof, sink) = setup(config(), &model, LiveMode::ApiKey, true).await;
    for (id, text, instructions) in [("d-one", "雪☃", "first"), ("d-two", "é✓", "second")] {
        assert_eq!(
            runner.run(event(id), instructions.into()).await.unwrap(),
            Some(commentary(id, text))
        );
        trace(&sink, id, 1);
        sink.0.lock().unwrap().clear();
    }
    assert_eq!(spoof.snapshot().unwrap().calls, calls(&["雪", "é"]));
    let requests = model.finish().await;
    assert_eq!(requests.len(), 4);
    for (index, id, instructions) in [(0, "d-one", "first"), (2, "d-two", "second")] {
        assert_eq!(requests[index].body["input"][0]["content"][0]["text"], instructions);
        assert_eq!(
            requests[index].body["input"][1]["content"][0]["text"],
            serde_json::to_value(event(id)).unwrap().to_string()
        );
    }
    drop(runner);
    spoof.shutdown().await.unwrap();
}
#[tokio::test]
async fn d11_model_failure_is_terminal() {
    let model = Model::start(vec![Reply {
        status: 500,
        body: "synthetic".into(),
        stall: false,
    }])
    .await;
    let (mut runner, spoof, sink) = setup(config(), &model, LiveMode::ApiKey, true).await;
    assert_eq!(
        runner.run(event("d-model"), "i".into()).await.unwrap(),
        Some(commentary("d-model", "model request failed"))
    );
    assert_eq!(model.finish().await.len(), 1);
    assert_eq!(spoof.snapshot().unwrap().calls, vec![]);
    record("D08/D11 exact request count=1 tools=0 terminal verified");
    trace(&sink, "d-model", 0);
    drop(runner);
    spoof.shutdown().await.unwrap();
}
