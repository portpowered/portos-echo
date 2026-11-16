use super::{
    model::{Model, Reply, final_item},
    support::*,
};
use echo_config::{AgentConfig, LiveMode, McpServer};
use echo_contracts::{
    ids::{DelegationId, SessionId},
    live::LiveEvent,
    secret::Secret,
};
use echo_delegation::{DelegationRunner, TraceEvent, TraceSink};
use echo_live::{
    LivePeer,
    call::{CallError, CredentialProvider, LiveAuthMode, LiveCall, LiveCallFactory, LiveCredential, SessionOptions},
    fake::{FakeLive, FakeResponse, FakeScript},
};
use echo_mcp_client::{McpClient, TokenError, TokenProvider};
use echo_spoof_mcp::{CallRecord, Fault, Options, Spoof};
use serde_json::{Value, json};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct Token;
impl TokenProvider for Token {
    fn token(&self) -> impl Future<Output = Result<Secret<String>, TokenError>> + Send {
        std::future::ready(Ok(Secret::new("synthetic-token".into())))
    }
}
struct LiveCredentials;
impl CredentialProvider for LiveCredentials {
    fn credential(&self) -> impl Future<Output = Result<LiveCredential, CallError>> + Send {
        std::future::ready(Ok(LiveCredential::ApiKey {
            key: Secret::new("synthetic-token".into()),
        }))
    }
}
#[derive(Default)]
pub struct Sink(pub Mutex<Vec<TraceEvent>>);
impl TraceSink for Sink {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}
pub type Runner = DelegationRunner<Token, Credentials, Sink>;
pub fn event(id: &str) -> LiveEvent {
    serde_json::from_value(json!({"type":"session.delegation.created","delegation":{"id":id,"target":"client","opaque":{"text":"雪"}},"unknown":[true,id]})).unwrap()
}
pub fn commentary(id: &str, text: &str) -> LiveEvent {
    LiveEvent::CommentaryAppend {
        delegation_id: DelegationId::new(id).unwrap(),
        text: text.into(),
        extra: std::collections::BTreeMap::default(),
    }
}
pub async fn setup(cfg: AgentConfig, model: &Model, mode: LiveMode, allowed: bool) -> (Runner, Spoof, Arc<Sink>) {
    let spoof = Spoof::start(Options {
        expected_bearer: Some(Secret::new("synthetic-token".into())),
        ..Options::default()
    })
    .await
    .unwrap();
    let host = Arc::new(
        McpClient::new(
            McpServer {
                url: spoof.url().into(),
                name: "spoof".into(),
                auth: echo_config::McpAuth::BearerFile,
                bearer_file: None,
                allow_tools: if allowed {
                    vec!["test.echo".into(), "test.wait".into()]
                } else {
                    vec![]
                },
            },
            Token,
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let sink = Arc::new(Sink::default());
    let runner = Runner::new(
        cfg,
        mode,
        host,
        Arc::new(Credentials::default()),
        sink.clone(),
        Some(&model.base),
    )
    .unwrap();
    (runner, spoof, sink)
}
pub fn function(index: usize, text: &str) -> Value {
    json!({"type":"function_call","call_id":format!("call-{index}"),"name":"tool_0","arguments":json!({"text":text}).to_string()})
}
pub fn calls(texts: &[&str]) -> Vec<CallRecord> {
    texts
        .iter()
        .enumerate()
        .map(|(i, text)| CallRecord {
            name: "test.echo".into(),
            args: json!({"text":text}),
            count: u64::try_from(i + 1).unwrap(),
        })
        .collect()
}
pub fn trace(sink: &Sink, id: &str, count: usize) {
    let mut expected = vec![TraceEvent::DelegationCreated {
        delegation_id: DelegationId::new(id).unwrap(),
    }];
    expected.extend((0..count).map(|_| TraceEvent::ToolCall {
        name: "test.echo".into(),
    }));
    expected.push(TraceEvent::CommentaryAppend {
        delegation_id: DelegationId::new(id).unwrap(),
    });
    assert_eq!(*sink.0.lock().unwrap(), expected);
    record(&format!("delegation={id} trace={expected:?}"));
}
pub async fn live(id: &str) -> (FakeLive, LiveCall) {
    let fake = FakeLive::bind(FakeScript {
        events: vec![serde_json::to_string(&event(id)).unwrap()],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let factory = LiveCallFactory::new(&fake.base_url(), LiveAuthMode::ApiKey, LiveCredentials).unwrap();
    let peer = Box::pin(LivePeer::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Instant::now(),
    ))
    .await
    .unwrap();
    let call = factory
        .create(
            peer,
            SessionOptions {
                model: "gpt-live-1".into(),
                voice: "synthetic".into(),
                instructions: "fixture".into(),
                session_id: SessionId::new("synthetic").unwrap(),
                thread_id: "synthetic".into(),
            },
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap();
    (fake, call)
}
#[tokio::test]
async fn d01_d02_d10_success_error_and_denial() {
    for (mode, fault, allowed, id) in [
        (LiveMode::Chatgpt, Fault::None, true, "d-echo"),
        (LiveMode::ApiKey, Fault::None, true, "d-echo"),
        (LiveMode::ApiKey, Fault::Http500, true, "d-error"),
        (LiveMode::ApiKey, Fault::None, false, "d-denied"),
    ] {
        let text = if fault == Fault::None && allowed {
            "test echo"
        } else {
            "I couldn't call test.echo"
        };
        let model = Model::start(vec![
            Reply::body(sse(&[function(0, "test echo")])),
            Reply::body(sse(&[final_item(text)])),
        ])
        .await;
        let (mut runner, spoof, sink) = setup(config(), &model, mode, allowed).await;
        spoof.set_fault(fault).unwrap();
        let (fake, mut call) = live(id).await;
        let input = call
            .receive_event(Instant::now() + Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(input, event(id));
        let output = runner.run(input, "Echo instructions".into()).await.unwrap().unwrap();
        assert_eq!(output, commentary(id, text));
        let LiveEvent::CommentaryAppend {
            delegation_id, text, ..
        } = output
        else {
            unreachable!()
        };
        call.append_commentary(delegation_id, text.clone()).await.unwrap();
        assert_eq!(
            call.receive_event(Instant::now() + Duration::from_secs(10))
                .await
                .unwrap(),
            LiveEvent::Other(json!({"type":"fake.commentary.received"}))
        );
        assert_delivery(&fake, id, &text);
        check_exchange(&model.finish().await, mode, id, fault, allowed);
        record(&format!(
            "D01/D02/D10 mode={mode:?} fault={fault:?} allowed={allowed} commentary={text:?} exact continuation verified"
        ));
        assert_eq!(
            spoof.snapshot().unwrap().calls,
            if allowed { calls(&["test echo"]) } else { vec![] }
        );
        trace(&sink, id, 1);
        call.close();
        drop(runner);
        fake.shutdown().await;
        spoof.shutdown().await.unwrap();
    }
}
fn check_exchange(requests: &[Request], mode: LiveMode, id: &str, fault: Fault, allowed: bool) {
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["model"], "fixture-model");
    assert_eq!(requests[0].body["input"][0]["content"][0]["text"], "Echo instructions");
    assert_eq!(
        requests[0].body["input"][1]["content"][0]["text"],
        serde_json::to_value(event(id)).unwrap().to_string()
    );
    let result = if !allowed {
        json!({"content":{"error":"denied"},"is_error":true})
    } else if fault == Fault::Http500 {
        json!({"content":{"error":"server error: http_status:500"},"is_error":true})
    } else {
        json!({"content":[{"type":"text","text":"test echo"}],"is_error":false})
    };
    assert_eq!(
        requests[1].body["input"][3],
        json!({"type":"function_call_output","call_id":"call-0","output":result.to_string()})
    );
    for r in requests {
        assert_eq!(
            r.start,
            if mode == LiveMode::Chatgpt {
                "POST /backend-api/codex/responses HTTP/1.1"
            } else {
                "POST /v1/responses HTTP/1.1"
            }
        );
        assert_eq!(r.headers.contains_key("chatgpt-account-id"), mode == LiveMode::Chatgpt);
    }
}

fn assert_delivery(fake: &FakeLive, id: &str, text: &str) {
    let received = &fake.observer().calls().unwrap()[0].commentary;
    assert_eq!(
        received
            .iter()
            .map(|s| serde_json::from_str::<LiveEvent>(s).unwrap())
            .collect::<Vec<_>>(),
        vec![commentary(id, text)]
    );
}

pub fn record(value: &str) {
    use std::io::Write;
    writeln!(std::io::stdout(), "{value} PASS").unwrap();
}
