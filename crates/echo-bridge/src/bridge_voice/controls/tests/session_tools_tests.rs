use super::super::{
    Notice, RunnerSkills,
    voice_tools::{Session, session_catalog},
};
use super::*;
use echo_agent_api::{ToolError, ToolHost, ToolSpec};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

const LISTEN: Duration = Duration::from_secs(3);
const MAX: Duration = Duration::from_secs(12);

/// A settled first reply one second after the run opened: listening to 4 s.
async fn listening() -> (Grants, Controller) {
    let grants = Grants::new(Some(LISTEN), MAX);
    let mut controller = Controller::default();
    controller
        .admit_response(Instant::now() + Duration::from_secs(10))
        .unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    controller.reply_started();
    controller.played(true).unwrap();
    controller.listen_after_reply(LISTEN).unwrap();
    (grants, controller)
}

type Receipts = Vec<(&'static str, &'static str, Option<u64>, Option<bool>)>;

fn ask(
    command: SessionCommand,
    key: &str,
    grants: &mut Grants,
    controller: &mut Controller,
    receipts: &mut Receipts,
) -> echo_agent_api::ToolResult {
    let (reply, mut answer) = oneshot::channel();
    let request = SessionRequest {
        command,
        key: key.into(),
        run: Some(grants.run.clone()),
        reply,
    };
    answer_one(request, grants, controller, receipts).unwrap();
    answer.try_recv().unwrap()
}

fn answer_one(
    request: SessionRequest,
    grants: &mut Grants,
    controller: &mut Controller,
    receipts: &mut Receipts,
) -> Result<(), Error> {
    answer(request, grants, controller, &mut |notice| {
        let Notice::SessionTool(receipt) = notice else {
            unreachable!()
        };
        receipts.push((receipt.tool, receipt.outcome, receipt.deadline_ms, receipt.capped));
        Ok(())
    })
}

#[tokio::test(start_paused = true)]
async fn session_tools_extend_is_capped_from_run_open_and_duplicates_never_mutate() {
    let (mut grants, mut controller) = listening().await;
    let opened = Instant::now() - Duration::from_secs(1);
    assert_eq!(controller.idle_deadline(), Some(opened + Duration::from_secs(4)));
    let mut receipts = Receipts::new();
    let first = ask(
        SessionCommand::Extend(20),
        "k1",
        &mut grants,
        &mut controller,
        &mut receipts,
    );
    assert_eq!(first.content, json!({"deadline_ms":12000,"capped":true}));
    assert!(!first.is_error);
    assert_eq!(controller.idle_deadline(), Some(opened + MAX));
    let second = ask(
        SessionCommand::Extend(5),
        "k2",
        &mut grants,
        &mut controller,
        &mut receipts,
    );
    assert_eq!(second, first);
    let again = ask(
        SessionCommand::Extend(1),
        "k1",
        &mut grants,
        &mut controller,
        &mut receipts,
    );
    assert_eq!(again, first);
    assert_eq!(
        receipts,
        [
            (EXTEND, "applied", Some(12000), Some(true)),
            (EXTEND, "applied", Some(12000), Some(true)),
            (EXTEND, "duplicate", None, None),
        ]
    );
    assert_eq!(grants.granted(), Some(opened + MAX));
}

#[tokio::test(start_paused = true)]
async fn session_tools_extend_adds_to_the_current_deadline_and_survives_the_next_reply() {
    let (mut grants, mut controller) = listening().await;
    let opened = Instant::now() - Duration::from_secs(1);
    let mut receipts = Receipts::new();
    let result = ask(
        SessionCommand::Extend(2),
        "k",
        &mut grants,
        &mut controller,
        &mut receipts,
    );
    assert_eq!(result.content, json!({"deadline_ms":6000,"capped":false}));
    // Activity starts a reply; the grant is retained and re-armed after it.
    controller.resume().unwrap();
    assert_eq!(controller.idle_deadline(), None);
    controller.extend_idle(opened + MAX);
    assert_eq!(controller.idle_deadline(), None);
    controller.reply_started();
    controller.played(true).unwrap();
    controller.listen_after_reply(LISTEN).unwrap();
    controller.extend_idle(grants.granted().unwrap());
    assert_eq!(controller.idle_deadline(), Some(opened + Duration::from_secs(6)));
    // During a reply the base is the projected post-play deadline.
    controller.resume().unwrap();
    let during = ask(
        SessionCommand::Extend(4),
        "k2",
        &mut grants,
        &mut controller,
        &mut receipts,
    );
    assert_eq!(during.content, json!({"deadline_ms":10000,"capped":false}));
}

#[tokio::test(start_paused = true)]
async fn session_tools_end_is_sticky_and_refuses_later_extension() {
    let (mut grants, mut controller) = listening().await;
    let deadline = controller.idle_deadline();
    let mut receipts = Receipts::new();
    let ended = ask(SessionCommand::End, "e1", &mut grants, &mut controller, &mut receipts);
    assert_eq!(ended.content, json!({"ending_after_reply":true}));
    assert!(grants.ending());
    let repeated = ask(SessionCommand::End, "e2", &mut grants, &mut controller, &mut receipts);
    assert_eq!(repeated, ended);
    let refused = ask(
        SessionCommand::Extend(5),
        "x",
        &mut grants,
        &mut controller,
        &mut receipts,
    );
    assert_eq!(refused.content, json!({"error":"session_closing"}));
    assert!(refused.is_error);
    assert_eq!(controller.idle_deadline(), deadline);
    assert_eq!(
        receipts,
        [
            (END, "applied", None, None),
            (END, "duplicate", None, None),
            (EXTEND, "session_closing", None, None),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn session_tools_invalid_inactive_and_closing_requests_never_mutate() {
    let (mut grants, mut controller) = listening().await;
    let deadline = controller.idle_deadline();
    let mut receipts = Receipts::new();
    for (command, key) in [
        (SessionCommand::Invalid { end: false }, "a"),
        (SessionCommand::Invalid { end: true }, "b"),
    ] {
        let result = ask(command, key, &mut grants, &mut controller, &mut receipts);
        assert_eq!(result.content, json!({"error":"invalid_arguments"}));
        assert!(result.is_error);
    }
    assert!(!grants.ending());
    assert_eq!(controller.idle_deadline(), deadline);
    let mut one_reply = Grants::new(None, MAX);
    let inactive = ask(SessionCommand::End, "c", &mut one_reply, &mut controller, &mut receipts);
    assert_eq!(inactive.content, json!({"error":"session_inactive"}));
    assert!(!one_reply.ending());
    controller.begin_close(true).unwrap();
    let closing = ask(
        SessionCommand::Extend(3),
        "d",
        &mut grants,
        &mut controller,
        &mut receipts,
    );
    assert_eq!(closing.content, json!({"error":"session_closing"}));
    assert_eq!(grants.granted(), None);
    assert_eq!(controller.idle_deadline(), None);
    assert_eq!(
        receipts,
        [
            (EXTEND, "invalid_arguments", None, None),
            (END, "invalid_arguments", None, None),
            (END, "session_inactive", None, None),
            (EXTEND, "session_closing", None, None),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn session_tools_failed_receipt_never_reports_success_and_idle_is_inactive() {
    let (mut grants, mut controller) = listening().await;
    let (reply, mut answer_rx) = oneshot::channel();
    let request = SessionRequest {
        command: SessionCommand::End,
        key: "k".into(),
        run: Some(grants.run.clone()),
        reply,
    };
    let result = answer(request, &mut grants, &mut controller, &mut |_| Err(Error::Trace));
    assert_eq!(result, Err(Error::Trace));
    assert!(answer_rx.try_recv().is_err());
    let (reply, mut answer_rx) = oneshot::channel();
    let request = SessionRequest {
        command: SessionCommand::Extend(3),
        key: "k2".into(),
        run: None,
        reply,
    };
    let mut seen = vec![];
    inactive(request, |notice| {
        let Notice::SessionTool(receipt) = notice else {
            unreachable!()
        };
        seen.push((receipt.tool, receipt.outcome));
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, [(EXTEND, "session_inactive")]);
    assert_eq!(
        answer_rx.try_recv().unwrap().content,
        json!({"error":"session_inactive"})
    );
}

#[derive(Clone, Default)]
struct Inner(Arc<Mutex<Vec<String>>>);

impl ToolHost for Inner {
    fn list_tools(&self) -> impl std::future::Future<Output = Result<Vec<ToolSpec>, ToolError>> {
        std::future::ready(Ok(vec![]))
    }
    fn call_tool(
        &self,
        name: &str,
        _: Value,
        _: &str,
    ) -> impl std::future::Future<Output = Result<echo_agent_api::ToolResult, ToolError>> {
        self.0.lock().unwrap().push(name.into());
        std::future::ready(Ok(echo_agent_api::ToolResult {
            content: json!("mcp"),
            is_error: false,
        }))
    }
}

#[tokio::test]
async fn session_tools_host_parses_strictly_and_routes_other_names_to_mcp() {
    let inner = Inner::default();
    let (requests, mut receiver) = mpsc::channel(1);
    let token = Arc::new(());
    let (_run, active) = tokio::sync::watch::channel(Some(token.clone()));
    let host = Session::new(inner.clone(), RunnerSkills::default(), requests, active);
    let cases = [
        (EXTEND, json!({"seconds":5}), SessionCommand::Extend(5)),
        (EXTEND, json!({"seconds":0}), SessionCommand::Invalid { end: false }),
        (EXTEND, json!({"seconds":1.5}), SessionCommand::Invalid { end: false }),
        (EXTEND, json!({"seconds":"5"}), SessionCommand::Invalid { end: false }),
        (
            EXTEND,
            json!({"seconds":5,"extra":1}),
            SessionCommand::Invalid { end: false },
        ),
        (EXTEND, json!(null), SessionCommand::Invalid { end: false }),
        (END, json!({}), SessionCommand::End),
        (END, json!({"now":true}), SessionCommand::Invalid { end: true }),
    ];
    for (name, args, expected) in cases {
        let call = host.call_tool(name, args, "key");
        let answer = async {
            let request = receiver.recv().await.unwrap();
            assert_eq!(request.command, expected);
            assert_eq!(request.key, "key");
            assert!(Arc::ptr_eq(request.run.as_ref().unwrap(), &token));
            request
                .reply
                .send(echo_agent_api::ToolResult {
                    content: json!("answered"),
                    is_error: false,
                })
                .unwrap();
        };
        let (result, ()) = tokio::join!(call, answer);
        assert_eq!(result.unwrap().content, json!("answered"));
    }
    assert_eq!(inner.0.lock().unwrap().len(), 0);
    assert_eq!(
        host.call_tool("test.echo", json!({}), "k").await.unwrap().content,
        json!("mcp")
    );
    assert_eq!(*inner.0.lock().unwrap(), ["test.echo"]);
    // A dropped run owner fails the tool rather than reporting success.
    drop(receiver);
    assert_eq!(host.call_tool(END, json!({}), "k").await, Err(ToolError::Transport));
}

fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: "synthetic".into(),
        input_schema: json!({"type":"object","properties":{}}),
    }
}

// T1: cancelling the waiting caller cannot commit a queued controller command.
#[tokio::test(start_paused = true)]
async fn session_tools_cancelled_queued_end_and_extend_leave_state_and_cache_unchanged() {
    let (mut grants, mut controller) = listening().await;
    let deadline = controller.idle_deadline();
    let mut receipts = Receipts::new();
    for command in [SessionCommand::End, SessionCommand::Extend(20)] {
        let (reply, receiver) = oneshot::channel();
        let request = SessionRequest {
            command,
            key: "cancelled".into(),
            run: Some(grants.run.clone()),
            reply,
        };
        drop(receiver);
        answer_one(request, &mut grants, &mut controller, &mut receipts).unwrap();
    }
    assert_eq!(controller.idle_deadline(), deadline);
    assert!(!grants.ending());
    assert_eq!(grants.granted(), None);
    assert_eq!(grants.answered.len(), 0);
    assert_eq!(
        receipts,
        [
            (END, "request_cancelled", None, None),
            (EXTEND, "request_cancelled", None, None)
        ]
    );
    let valid = ask(
        SessionCommand::End,
        "cancelled",
        &mut grants,
        &mut controller,
        &mut receipts,
    );
    assert!(!valid.is_error);
    assert!(grants.ending());
}

#[tokio::test(start_paused = true)]
async fn session_tools_old_run_delivery_after_new_wake_is_refused_without_effects() {
    let (mut grants, mut controller) = listening().await;
    let old = grants.run.clone();
    grants = Grants::new(Some(LISTEN), MAX);
    // No clock advance is needed to distinguish two admissions.
    let deadline = controller.idle_deadline();
    let mut receipts = Receipts::new();
    for command in [SessionCommand::End, SessionCommand::Extend(20)] {
        let (reply, mut receiver) = oneshot::channel();
        let request = SessionRequest {
            command,
            key: "old".into(),
            run: Some(old.clone()),
            reply,
        };
        answer_one(request, &mut grants, &mut controller, &mut receipts).unwrap();
        assert_eq!(receiver.try_recv().unwrap().content, json!({"error":"session_stale"}));
    }
    assert_eq!(controller.idle_deadline(), deadline);
    assert!(!grants.ending());
    assert_eq!(grants.granted(), None);
    assert_eq!(grants.answered.len(), 0);
    assert_eq!(
        receipts,
        [
            (END, "session_stale", None, None),
            (EXTEND, "session_stale", None, None)
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn session_tools_cancelled_dispatch_before_controller_delivery_cannot_apply() {
    use echo_codex::codex_protocol::ToolCall;
    use echo_delegation::codex_dispatch::Dispatcher;
    let (mut grants, mut controller) = listening().await;
    let deadline = controller.idle_deadline();
    let (requests, mut receiver) = mpsc::channel(1);
    let (_run, active) = tokio::sync::watch::channel(Some(grants.run.clone()));
    let host = Session::new(Inner::default(), RunnerSkills::default(), requests, active);
    let catalog = session_catalog(vec![], &[]).unwrap();
    let mut dispatcher = Dispatcher::new(host, catalog, "thread".into()).unwrap();
    dispatcher.start_turn("thread", "turn").unwrap();
    let call = ToolCall {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        call_id: "call".into(),
        tool: END.into(),
        arguments: json!({}),
        namespace: None,
    };
    let request = {
        let pending = dispatcher.dispatch(&call);
        tokio::pin!(pending);
        tokio::select! {
            request = receiver.recv() => request.unwrap(),
            _ = &mut pending => unreachable!("dispatch must await the controller"),
        }
    };
    assert!(request.reply.is_closed());
    let mut receipts = Receipts::new();
    answer_one(request, &mut grants, &mut controller, &mut receipts).unwrap();
    assert_eq!(controller.idle_deadline(), deadline);
    assert!(!grants.ending());
    assert_eq!(grants.granted(), None);
    assert_eq!(grants.answered.len(), 0);
    assert_eq!(receipts, [(END, "request_cancelled", None, None)]);
}

#[test]
fn session_tools_catalog_appends_exact_builtins_and_refuses_shadowing() {
    let allowed = vec!["test.echo".to_owned()];
    let catalog = session_catalog(vec![spec("test.echo")], &allowed).unwrap();
    let names: Vec<_> = catalog.tools().iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["test_echo", EXTEND, END]);
    assert_eq!(catalog.resolve(EXTEND), Some(EXTEND));
    assert_eq!(catalog.resolve(END), Some(END));
    // An unallowed similar name is filtered; an exact duplicate never is.
    session_catalog(vec![spec("end.session")], &allowed).unwrap();
    for (discovered, allow) in [(EXTEND, "test.echo"), ("end.session", "end.session")] {
        let allowed = vec![allow.to_owned()];
        assert_eq!(
            session_catalog(vec![spec(discovered)], &allowed).err(),
            Some(Error::Input)
        );
    }
}

#[test]
fn session_tools_options_reject_a_zero_or_unrepresentable_session_cap() {
    let mut options = super::super::SessionOptions {
        listen_timeout: Some(LISTEN),
        session_max: MAX,
        idle: Duration::from_secs(5),
        response_timeout: Duration::from_secs(10),
        played_timeout: Duration::from_secs(1),
        reply_lead_ms: 200,
        muted: false,
    };
    super::super::voice_session::validate(&options).unwrap();
    for session_max in [Duration::ZERO, Duration::MAX] {
        options.session_max = session_max;
        assert_eq!(super::super::voice_session::validate(&options), Err(Error::Input));
    }
}
