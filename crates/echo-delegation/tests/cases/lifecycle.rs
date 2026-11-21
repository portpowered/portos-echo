use super::{
    journey::*,
    model::{Model, Reply},
};
use crate::{config, sse};
use echo_config::LiveMode;
use echo_delegation::TraceEvent;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn d06_d12_stalled_model_cancel_and_abandon() {
    for abandon in [false, true] {
        let mut model = Model::start(vec![Reply::stalled()]).await;
        let mut cfg = config();
        cfg.task_deadline_seconds = 5;
        let (mut runner, spoof, sink) = setup(cfg, &model, LiveMode::ApiKey, true).await;
        {
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            let run = runner.run_until_cancelled(event("d-stall"), "i".into(), async {
                cancelled.await.unwrap();
            });
            tokio::pin!(run);
            tokio::select! { result=&mut run => panic!("unexpected terminal {result:?}"), ()=model.entered()=>() }
            if !abandon {
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(5)).await;
                cancel.send(()).unwrap();
                assert_eq!(run.await.unwrap(), None);
                tokio::time::resume();
            }
        }
        drop(runner);
        model.closed().await;
        record(&format!(
            "D06/D12 abandon={abandon} model disconnected tools=0 terminal verified"
        ));
        assert_eq!(model.finish().await.len(), 1);
        assert_eq!(spoof.snapshot().unwrap().calls, vec![]);
        assert_eq!(
            sink.0.lock().unwrap().iter().map(TraceEvent::name).collect::<Vec<_>>(),
            vec!["session.delegation.created"]
        );
        spoof.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn d07_received_tool_effect_survives_owner_cancellation() {
    let model = Model::start(vec![Reply::body(sse(&[
        json!({"type":"function_call","call_id":"call-wait","name":"tool_1","arguments":"{\"ms\":10000}"}),
    ]))])
    .await;
    let mut cfg = config();
    cfg.task_deadline_seconds = 5;
    let (mut runner, spoof, sink) = setup(cfg, &model, LiveMode::ApiKey, true).await;
    {
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let run = runner.run_until_cancelled(event("d-wait"), "i".into(), async {
            cancelled.await.unwrap();
        });
        tokio::pin!(run);
        tokio::select! { result=&mut run => panic!("unexpected terminal {result:?}"), result=spoof.wait_for_calls(1)=>result.unwrap() }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(5)).await;
        cancel.send(()).unwrap();
        assert_eq!(run.await.unwrap(), None);
        tokio::time::resume();
    }
    assert_eq!(model.finish().await.len(), 1);
    let snapshot = spoof.snapshot().unwrap();
    assert_eq!(snapshot.calls.len(), 1);
    assert_eq!(snapshot.calls[0].name, "test.wait");
    assert_eq!(snapshot.calls[0].args, json!({"ms":10000}));
    assert_eq!(
        sink.0.lock().unwrap().iter().map(TraceEvent::name).collect::<Vec<_>>(),
        vec!["session.delegation.created", "tools/call"]
    );
    record("D07 test.wait ms=10000 arrivals=1 model_posts=1 commentary=0");
    drop(runner);
    spoof.shutdown().await.unwrap();
}
