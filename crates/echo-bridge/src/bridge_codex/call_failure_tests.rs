//! T1: failure/cancellation boundaries on controlled memory streams.
use super::{call_tests::*, *};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

#[tokio::test(start_paused = true)]
async fn call_omitted_voice_uses_server_default_without_null_or_legacy_substitution() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        let options = Options {
            voice: None,
            ..options()
        };
        assert_eq!(
            call.start(&options, vec![], "fixture-offer", deadline()).await.unwrap(),
            "fixture-answer"
        );
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        thread(&mut server).await;
        let request = read(&mut server).await;
        assert_eq!(request["method"], "thread/realtime/start");
        assert_eq!(
            request["params"],
            json!({
                "threadId":"thread-1", "outputModality":"audio", "transport":{"type":"webrtc","sdp":"fixture-offer"},
                "version":"v3", "includeStartupContext":false, "clientManagedHandoffs":false,
                "prompt":"Live prompt", "initialItems":[{"role":"developer","text":"Live prompt"}]
            })
        );
        notification(
            &mut server,
            "thread/realtime/sdp",
            json!({"threadId":"thread-1","sdp":"fixture-answer"}),
        )
        .await;
        write(&mut server, json!({"id":request["id"],"result":{}})).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn call_empty_offer_has_no_effect_and_omitted_model_stays_omitted() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        assert!(matches!(call.next().await, Err(Error::Unavailable)));
        assert_eq!(
            call.start(&options(), vec![], "  ", deadline()).await,
            Err(Error::Input)
        );
        let options = Options {
            model: None,
            ..options()
        };
        assert_eq!(
            call.start(&options, vec![], "fixture-offer", deadline()).await.unwrap(),
            "fixture-answer"
        );
        assert!(matches!(call.next().await.unwrap(), Event::Closed));
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        let params = start(&mut server).await;
        assert_eq!(params.get("model"), None);
        notification(&mut server, "thread/realtime/closed", json!({"threadId":"thread-1"})).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn call_bad_thread_or_remote_failure_closes_before_realtime_start() {
    for result in [
        json!({}),
        json!({"thread":{"id":""}}),
        json!({"thread":{"id":"bad\nid"}}),
    ] {
        let (mut io, mut server) = fixture();
        let client = async {
            let mut call = Call::new(&mut io);
            assert_eq!(
                call.start(
                    &Options {
                        effort: Some("low"),
                        fast: false,
                        ..options()
                    },
                    vec![],
                    "fixture-offer",
                    deadline()
                )
                .await,
                Err(Error::Decode)
            );
        };
        let remote = async {
            let request = read(&mut server).await;
            write(&mut server, json!({"id":request["id"],"result":result})).await;
            let mut line = String::new();
            assert_eq!(server.read_line(&mut line).await.unwrap(), 0);
        };
        tokio::join!(client, remote);
    }
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        assert_eq!(
            call.start(
                &Options {
                    effort: Some("low"),
                    fast: false,
                    ..options()
                },
                vec![],
                "fixture-offer",
                deadline()
            )
            .await,
            Err(Error::Unavailable)
        );
    };
    let remote = async {
        let request = read(&mut server).await;
        write(
            &mut server,
            json!({"id":request["id"],"error":{"code":-32000,"message":"private"}}),
        )
        .await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn call_sdp_errors_early_close_and_early_overflow_fail_explicitly() {
    for case in ["empty", "missing", "closed", "overflow"] {
        let (mut io, mut server) = fixture();
        let client = async {
            let mut call = Call::new(&mut io);
            let expected = if matches!(case, "empty" | "missing") {
                Error::Decode
            } else {
                Error::Unavailable
            };
            assert_eq!(
                call.start(&options(), vec![], "fixture-offer", deadline()).await,
                Err(expected)
            );
        };
        let remote = async {
            thread(&mut server).await;
            let request = read(&mut server).await;
            write(&mut server, json!({"id":request["id"],"result":{}})).await;
            match case {
                "empty" => {
                    notification(
                        &mut server,
                        "thread/realtime/sdp",
                        json!({"threadId":"thread-1","sdp":" "}),
                    )
                    .await;
                }
                "missing" => notification(&mut server, "thread/realtime/sdp", json!({"threadId":"thread-1"})).await,
                "closed" => notification(&mut server, "thread/realtime/closed", json!({"threadId":"thread-1"})).await,
                "overflow" => {
                    for n in 0..33 {
                        notification(
                            &mut server,
                            "thread/realtime/transcript/delta",
                            json!({"threadId":"thread-1","role":"assistant","delta":n.to_string()}),
                        )
                        .await;
                    }
                }
                _ => unreachable!(),
            }
        };
        tokio::join!(client, remote);
        assert_eq!(
            io.begin_request("account/read", &json!({})),
            Err(RpcError::Disconnected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn call_eof_or_malformed_matching_transcript_closes_active_generation() {
    for malformed in [true, false] {
        let (mut io, mut server) = fixture();
        let client = async {
            let mut call = Call::new(&mut io);
            call.start(&options(), vec![], "fixture-offer", deadline())
                .await
                .unwrap();
            let expected = if malformed { Error::Decode } else { Error::Unavailable };
            assert!(matches!(call.next().await, Err(error) if error == expected));
        };
        let remote = async move {
            start(&mut server).await;
            if malformed {
                notification(
                    &mut server,
                    "thread/realtime/transcript/done",
                    json!({"threadId":"thread-1","role":"assistant"}),
                )
                .await;
            }
        };
        tokio::join!(client, remote);
        assert_eq!(
            io.begin_request("account/read", &json!({})),
            Err(RpcError::Disconnected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn call_stop_cancellation_and_abandoned_active_call_close_io() {
    for cancel_stop in [true, false] {
        let (mut io, mut server) = fixture();
        let client = async {
            let mut call = Call::new(&mut io);
            call.start(&options(), vec![], "fixture-offer", deadline())
                .await
                .unwrap();
            if cancel_stop {
                assert!(
                    tokio::time::timeout(Duration::from_secs(1), call.stop(deadline()))
                        .await
                        .is_err()
                );
            }
            drop(call);
        };
        let remote = async {
            start(&mut server).await;
            if cancel_stop {
                assert_eq!(read(&mut server).await["method"], "thread/realtime/stop");
            }
            let mut line = String::new();
            assert_eq!(server.read_line(&mut line).await.unwrap(), 0);
        };
        tokio::join!(client, remote);
        assert_eq!(
            io.begin_request("account/read", &json!({})),
            Err(RpcError::Disconnected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn call_stopping_before_start_is_rejected_without_a_request() {
    let (mut io, _server) = fixture();
    let mut call = Call::new(&mut io);
    assert_eq!(call.stop(deadline()).await, Err(Error::Unavailable));
    assert_eq!(call.stop(deadline()).await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn call_tool_results_preserve_original_ids_and_reject_repeat_answers() {
    let (mut io, mut server) = fixture();
    let result = ToolResult {
        content_items: vec![echo_codex::codex_protocol::ToolContent::InputText { text: "echoed".into() }],
        success: true,
    };
    let client = async {
        let mut call = Call::new(&mut io);
        assert_eq!(
            call.answer_tool(&RequestId::Number(42), &result),
            Err(Error::Unavailable)
        );
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        for expected in [RequestId::Number(42), RequestId::Text("request-id".into())] {
            let tool = match call.next().await.unwrap() {
                Event::Tool { id, call } => Some((id, call)),
                _ => None,
            };
            let (id, request) = tool.unwrap();
            assert_eq!(id, expected);
            assert_eq!(request.turn_id, "turn-1");
            assert_eq!(request.tool, "tool_0");
            assert_eq!(request.arguments, json!({"text":"echoed"}));
            call.answer_tool(&id, &result).unwrap();
            assert_eq!(call.answer_tool(&id, &result), Err(Error::Unavailable));
        }
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        for id in [json!(42), json!("request-id")] {
            write(&mut server, json!({"id":id,"method":"item/tool/call","params":{
                "threadId":"thread-1","turnId":"turn-1","callId":"call-1","tool":"tool_0","arguments":{"text":"echoed"}
            }})).await;
        }
        for id in [json!(42), json!("request-id")] {
            assert_eq!(
                read(&mut server).await,
                json!({"id":id,"result":{
                    "contentItems":[{"type":"inputText","text":"echoed"}],"success":true
                }})
            );
        }
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn call_malformed_tool_request_never_reaches_dispatcher() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        assert!(matches!(call.next().await, Err(Error::Decode)));
    };
    let remote = async {
        start(&mut server).await;
        write(
            &mut server,
            json!({"id":42,"method":"item/tool/call","params":{"threadId":"thread-1"}}),
        )
        .await;
    };
    tokio::join!(client, remote);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(RpcError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn call_matching_error_is_sanitized_and_foreign_error_is_ignored() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        assert!(matches!(call.next().await.unwrap(), Event::Other));
        assert!(matches!(call.next().await, Err(Error::Unavailable)));
    };
    let remote = async {
        start(&mut server).await;
        for thread in ["foreign", "thread-1"] {
            notification(
                &mut server,
                "thread/realtime/error",
                json!({"threadId":thread,"message":"private provider payload"}),
            )
            .await;
        }
    };
    tokio::join!(client, remote);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(RpcError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn call_profile_fields_are_independent_and_unset_request_is_unchanged() {
    for web_search in [
        echo_config::WebSearch::Live,
        echo_config::WebSearch::Cached,
        echo_config::WebSearch::Disabled,
    ] {
        for (model, effort) in [
            (None, None),
            (Some("fixture-model"), Some("low")),
            (None, Some("low")),
            (Some("fixture-model"), None),
        ] {
            let (mut io, mut server) = fixture();
            let client = async {
                let mut call = Call::new(&mut io);
                let options = Options {
                    model,
                    effort,
                    time_zone: Some("Europe/Berlin"),
                    web_search,
                    ..options()
                };
                assert_eq!(
                    call.start(&options, vec![], "fixture-offer", deadline()).await.unwrap(),
                    "fixture-answer"
                );
                call.stop(deadline()).await.unwrap();
            };
            let remote = async {
                // start asserts the full realtime request, including the absence of profile/zone keys.
                let params = start(&mut server).await;
                let mut expected = json!({
                    "ephemeral": true, "approvalPolicy": "never", "sandbox": "read-only",
                    "environments": [], "dynamicTools": [], "baseInstructions": "Client audio bridge",
                    "config": {
                        "features.shell_tool": false, "features.unified_exec": false,
                        "features.multi_agent": false, "features.multi_agent_v2": false, "agents.enabled": false,
                        "features.goals": false, "tools.experimental_request_user_input.enabled": false,
                        "tools.update_plan.enabled": false,
                "features.code_mode.direct_only_tool_namespaces": ["web", "functions"], "web_search": web_search.as_str(), "mcp_servers": {}
                    }
                });
                if let Some(model) = model {
                    expected["model"] = model.into();
                }
                if let Some(effort) = effort {
                    expected["config"]["model_reasoning_effort"] = effort.into();
                }
                assert_eq!(params, expected);
                stop(&mut server).await;
            };
            tokio::join!(client, remote);
            assert!(io.begin_request("account/read", &json!({})).is_ok());
        }
    }
}
