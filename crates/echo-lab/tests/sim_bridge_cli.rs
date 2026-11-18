//! T3 component witnesses through the prebuilt bridge executable and loopback.
//! No test builds a binary, uses hardware, or contacts a provider.
#[cfg(test)]
#[path = "sim_bridge_support/bridge_host.rs"]
mod bridge_host;
#[cfg(test)]
mod sim_bridge_support;

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::sim_bridge_support::{Session, receive, send};
    use echo_contracts::wyoming::Decoder;
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn t3_bridge_counts_two_actual_runs_without_microphone_audio_start() {
        let session = Session::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(listener.local_addr().unwrap().ip().to_string(), "127.0.0.1");
        let mut bridge = session.start(listener.local_addr().unwrap());
        let (mut peer, connected) = listener.accept().await.unwrap();
        assert_eq!(connected.ip().to_string(), "127.0.0.1");
        let mut decoder = Decoder::new();
        assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "run-satellite");
        assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "describe");
        send(&mut peer, "info", json!({"satellite":{"name":"test"}}), vec![]).await;
        assert_eq!(bridge.line().await, json!({"event":"bridge.ready"}));
        for index in 1..=2 {
            send(
                &mut peer,
                "run-pipeline",
                json!({
                    "start_stage":"asr", "end_stage":"tts", "restart_on_end":false, "wake_word_name":"alexa"
                }),
                vec![],
            )
            .await;
            for _ in 0..10 {
                send(
                    &mut peer,
                    "audio-chunk",
                    json!({"rate":16000,"width":2,"channels":1}),
                    vec![3; 2048],
                )
                .await;
            }
            assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "audio-start");
            for _ in 0..10 {
                let chunk = receive(&mut peer, &mut decoder).await;
                assert_eq!(chunk.header.r#type, "audio-chunk");
                assert_eq!(chunk.header.data, Some(json!({"rate":24000,"width":2,"channels":1})));
                assert_eq!(chunk.payload, vec![0; 480]);
            }
            assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "audio-stop");
            send(&mut peer, "played", Value::Null, vec![]).await;
            let end = receive(&mut peer, &mut decoder).await;
            assert_eq!(
                serde_json::to_value(end.header).unwrap(),
                json!({"type":"hassmic-end","data":null,"data_length":0,"payload_length":0})
            );
            assert_eq!(end.data, None);
            assert_eq!(end.payload, [] as [u8; 0]);
            assert_eq!(
                bridge.line().await,
                json!({"event":"bridge.run_completed","run_id":format!("run-{index}")})
            );
        }
        bridge.finish(0).await;
        let trace = session.trace();
        assert_eq!(trace.len(), 6);
        assert_traces(&trace);
    }

    fn assert_traces(trace: &[Value]) {
        for (index, run) in trace.as_chunks::<3>().0.iter().enumerate() {
            assert_eq!(
                run.iter().map(|v| v["event"].as_str().unwrap()).collect::<Vec<_>>(),
                ["wyoming.run_started", "wyoming.mic_frames", "wyoming.run_ended"]
            );
            for record in run {
                assert_eq!(record["run_id"], format!("run-{}", index + 1));
                assert!(record["elapsed_ms"].as_u64().is_some());
            }
            assert_eq!(run[1]["count"], 10);
        }
        assert!(
            trace
                .windows(2)
                .all(|pair| pair[0]["elapsed_ms"].as_u64() <= pair[1]["elapsed_ms"].as_u64())
        );
    }

    #[tokio::test]
    async fn t3_bridge_rejects_a_short_microphone_run_without_fabricating_completion() {
        let session = Session::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut bridge = session.start(listener.local_addr().unwrap());
        let (mut peer, _) = listener.accept().await.unwrap();
        let mut decoder = Decoder::new();
        assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "run-satellite");
        assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "describe");
        send(&mut peer, "info", json!({}), vec![]).await;
        assert_eq!(bridge.line().await, json!({"event":"bridge.ready"}));
        send(
            &mut peer,
            "run-pipeline",
            json!({
                "start_stage":"asr", "end_stage":"tts", "restart_on_end":false,"wake_word_name":"alexa"
            }),
            vec![],
        )
        .await;
        send(&mut peer, "audio-stop", Value::Null, vec![]).await;
        let diagnostic = bridge.finish(1).await;
        assert!(
            diagnostic.contains("microphone ended before ten frames"),
            "{diagnostic}"
        );
        assert_eq!(
            session
                .trace()
                .iter()
                .map(|v| v["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["wyoming.run_started"]
        );
    }

    #[test]
    fn invalid_address_and_existing_trace_fail_before_connecting() {
        let session = Session::new();
        for invalid in ["localhost:16700", "[::1]:16700", "127.0.0.1:0"] {
            let output = session
                .command()
                .env("ECHO_LAB_WYOMING_ADDR", invalid)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(output.stdout, [] as [u8; 0]);
            assert!(String::from_utf8(output.stderr).unwrap().contains("Wyoming address"));
            assert!(!session.0.join("trace.jsonl").exists());
        }
        std::fs::write(session.0.join("trace.jsonl"), b"preserved").unwrap();
        let output = session
            .command()
            .env("ECHO_LAB_WYOMING_ADDR", "127.0.0.1:16700")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("cannot create fresh trace")
        );
        assert_eq!(std::fs::read(session.0.join("trace.jsonl")).unwrap(), b"preserved");
    }

    #[tokio::test]
    async fn t3_bridge_eof_closes_its_registered_connection() {
        let session = Session::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut bridge = session.start(listener.local_addr().unwrap());
        let (mut peer, _) = listener.accept().await.unwrap();
        let mut decoder = Decoder::new();
        assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "run-satellite");
        assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "describe");
        send(&mut peer, "info", json!({}), vec![]).await;
        assert_eq!(bridge.line().await, json!({"event":"bridge.ready"}));
        bridge.finish(0).await;
        assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
        assert_eq!(session.trace(), Vec::<Value>::new());
        peer.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "T3 prebuilt pinned host: run with make test-sim"]
    async fn t3_bridge_observes_real_pinned_host_run() {
        let session = Session::new();
        let (mut host, port) = crate::bridge_host::Host::start();
        let mut bridge = session.start(([127, 0, 0, 1], port).into());
        assert_eq!(bridge.line().await, json!({"event":"bridge.ready"}));
        host.wake();
        assert_eq!(
            bridge.line().await,
            json!({"event":"bridge.run_completed","run_id":"run-1"})
        );
        bridge.finish(0).await;
        host.finish();
        let trace = session.trace();
        assert_eq!(trace.len(), 3);
        for (index, run) in trace.as_chunks::<3>().0.iter().enumerate() {
            assert_eq!(
                run.iter().map(|v| v["event"].as_str().unwrap()).collect::<Vec<_>>(),
                ["wyoming.run_started", "wyoming.mic_frames", "wyoming.run_ended"]
            );
            for record in run {
                assert_eq!(record["run_id"], format!("run-{}", index + 1));
            }
            assert!(run[1]["count"].as_u64().unwrap() >= 10);
            writeln!(
                std::io::stdout(),
                "pinned host run-{}: frames={} ordered=start,frames,end",
                index + 1,
                run[1]["count"]
            )
            .unwrap();
        }
    }
}
