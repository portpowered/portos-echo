use super::*;

fn content() -> serde_json::Value {
    json!([{"type":"text","text":json!({"state":"emotion","name":"happy",
        "frame_ms":80,"frames":["(^_^)","(^o^)"]}).to_string()}])
}

#[tokio::test(start_paused = true)]
async fn m6_sink_ignores_errors_and_bounds_full_and_closed_queues_without_content_logs() {
    let (send, mut receive) = mpsc::channel(32);
    let sink = Sink {
        send,
        overflow: AtomicBool::new(false),
    };
    sink.tool_result("ui.show_emotion", &content(), true);
    sink.tool_result("test.echo", &content(), false);
    assert!(receive.try_recv().is_err());
    for _ in 0..32 {
        sink.tool_result("ui.show_emotion", &content(), false);
    }
    assert!(!sink.overflow.load(Ordering::Relaxed));
    sink.tool_result("ui.show_emotion", &content(), false);
    assert!(sink.overflow.load(Ordering::Relaxed));
    for _ in 0..32 {
        let Observation::Emotion(state, deadline) = receive.try_recv().unwrap() else {
            unreachable!()
        };
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            json!({"state":"emotion","name":"happy",
            "frame_ms":80,"frames":["(^_^)","(^o^)"]})
        );
        assert_eq!(deadline, tokio::time::Instant::now() + Duration::from_millis(160));
    }
    assert!(receive.try_recv().is_err());
    sink.overflow.store(false, Ordering::Relaxed);
    drop(receive);
    sink.tool_result("ui.show_emotion", &content(), false);
    assert!(sink.overflow.load(Ordering::Relaxed));
}

#[tokio::test(start_paused = true)]
async fn m6_result_wakes_owner_before_terminal_and_abandon_discards_observation() {
    let (send, receive) = mpsc::channel(32);
    let sink = Arc::new(Sink {
        send,
        overflow: AtomicBool::new(false),
    });
    let mut host = Host {
        grant: None,
        config: None,
        model_url: None,
        budget: Arc::new(Budget::new(0)),
        utc_day: Arc::new(|| 0),
        credentials: None,
        runner: None,
        pending: Some(tokio::spawn(std::future::pending())),
        completed: None,
        sink: sink.clone(),
        receive,
        observed: None,
        seen: HashSet::new(),
        active: Some(("run-1".into(), "d-1".into())),
    };
    sink.tool_result("ui.show_emotion", &content(), false);
    host.ready().await;
    assert!(host.completed.is_none());
    assert!(!host.pending.as_ref().unwrap().is_finished());
    assert!(matches!(host.observed, Some(Observation::Emotion(_, _))));
    assert!(host.abandon());
    assert!(host.observed.is_none());
    assert!(host.active.is_none());
    assert!(host.pending.is_none());
    assert!(host.receive.try_recv().is_err());
    assert!(!host.abandon());
}
