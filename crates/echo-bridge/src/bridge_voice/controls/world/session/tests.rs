use super::*;
use echo_core_api::{SessionTurnEvent, Subsystem, ToolCallHandle};
use std::num::NonZeroU64;

#[tokio::test(start_paused = true)]
async fn session_world_wake_opens_only_a_new_call_and_scopes_its_receipt() {
    let world = Controller::new();
    let opened = Instant::now().into_std();
    let due = opened + Duration::from_secs(10);
    let max = Duration::from_secs(60);
    let wake = |session, reuse_call| SessionEvent::Wake {
        session,
        opened,
        listen: Some(Duration::from_secs(15)),
        max,
        response_due: due,
        reuse_call,
    };
    let session = SessionHandle {
        index: 0,
        generation: NonZeroU64::MIN,
    };
    let next = SessionHandle {
        generation: session.generation.checked_add(1).unwrap(),
        ..session
    };
    let mut state = world.state();
    let mut out = world.session_step(&mut state, wake(session, false));
    assert!(matches!(out.take_effect(), Some(SessionCommand::StartCapture { session: handle }) if handle == session));
    assert!(matches!(out.take_effect(), Some(SessionCommand::OpenCall { session: handle }) if handle == session));
    assert!(out.take_effect().is_none());
    assert!(!state.world.pacer.session.view().accepts_reply_audio);
    assert_eq!(state.world.pacer.session.view().next_deadline, None);

    let stale = world.session_step(&mut state, SessionEvent::CallOpened { session: next });
    assert_eq!(stale.outcome, Some(Err(SessionError::Control)));
    assert!(!state.world.pacer.session.view().accepts_reply_audio);
    let opened = world.session_step(&mut state, SessionEvent::CallOpened { session });
    assert_eq!(opened.outcome, Some(Ok(SessionOutcome::Applied)));
    assert!(state.world.pacer.session.view().accepts_reply_audio);
    assert_eq!(
        state.world.pacer.session.view().next_deadline,
        Some((due, SessionTimer::Response))
    );
    world.session_step(&mut state, SessionEvent::RunReleased { session });

    let mut out = world.session_step(&mut state, wake(next, true));
    assert!(matches!(out.take_effect(), Some(SessionCommand::StartCapture { session }) if session == next));
    assert!(out.take_effect().is_none());
    assert!(state.world.pacer.session.view().accepts_reply_audio);
}

#[tokio::test(start_paused = true)]
async fn session_slots_release_invalidates_retained_tool_results() {
    let world = Controller::new();
    let run = world.clone().admit_fixture(Instant::now(), None).unwrap();
    run.configure_tools(Some(Duration::from_secs(3)), Duration::from_secs(12))
        .unwrap();
    let (decision, cached) = run.answer_tool("key".into(), SessionToolAction::End).unwrap();
    let SessionToolDecision::Ending {
        tool: old,
        duplicate: false,
    } = decision
    else {
        unreachable!()
    };
    assert!(!cached);
    drop(run);

    let run = world.admit_fixture(Instant::now(), None).unwrap();
    run.configure_tools(Some(Duration::from_secs(3)), Duration::from_secs(12))
        .unwrap();
    let (decision, cached) = run
        .answer_tool("key".into(), SessionToolAction::Extend { seconds: 2 })
        .unwrap();
    let SessionToolDecision::Extended {
        tool: new,
        deadline_ms: 5000,
        capped: false,
    } = decision
    else {
        unreachable!()
    };
    assert!(!cached);
    assert_eq!(new.index, old.index);
    assert_ne!(new.generation, old.generation);
    assert!(!run.ending());
}

#[tokio::test(start_paused = true)]
async fn session_world_release_clears_quiet_deadline() {
    let world = Controller::new();
    let session = world.clone().admit_fixture(Instant::now(), None).unwrap();
    let window = Duration::from_secs(15);
    session.reply_started();
    session.played(true).unwrap();
    session.listen_after_reply(window).unwrap();
    let due = session.idle_deadline().unwrap();
    assert_eq!(
        session.view().next_deadline,
        Some((due.into_std(), SessionTimer::Listen))
    );
    assert!(!session.deadline_reached(SessionTimer::Listen));
    tokio::time::advance(window).await;
    assert!(session.deadline_reached(SessionTimer::Listen));
    session.begin_close(true).unwrap();
    assert_eq!(session.begin_close(true), Err(Error::Wyoming));
    assert_eq!(session.finish_close(true), Err(Error::Wyoming));
    session.microphone_ended();
    session.finish_close(true).unwrap();
    drop(session);

    let view = world.state().world.pacer.session.view();
    assert_eq!(view.session, None);
    assert_eq!(view.next_deadline, None);
    assert_eq!(view.idle_deadline, None);
    assert_eq!(
        echo_sub_session::Session::next_deadline(&world.state().world.pacer.session),
        None
    );
    assert!(world.state().world.check().is_ok());
}

#[test]
fn session_world_consumes_a_delayed_cut_receipt_while_closing() {
    let session = SessionPort::open(Instant::now(), None).unwrap();
    session.reply_started();
    session.cut();
    session.cut_settled();
    session.cut_played();
    assert_eq!(session.pending_cut_receipts(), 1);
    session.begin_close(true).unwrap();
    assert_eq!(session.played(true), Ok(false));
    assert_eq!(session.pending_cut_receipts(), 0);
    assert_eq!(session.played(true), Err(Error::Wyoming));
    session.microphone_ended();
    session.finish_close(true).unwrap();
    assert!(session.complete(true, true));
}

#[test]
fn session_slots_reuse_run_index_without_reopening_stale_run() {
    let world = Controller::new();
    let first = world.clone().admit_fixture(Instant::now(), None).unwrap();
    let old_handle = first.handle;
    assert!(matches!(
        world.clone().admit_fixture(Instant::now(), None),
        Err(Error::Control)
    ));
    drop(first);

    let second = world.clone().admit_fixture(Instant::now(), None).unwrap();
    assert_eq!(second.handle.index, old_handle.index);
    assert_ne!(second.handle.generation, old_handle.generation);
    let mut state = world.state();
    let result = world.session_step(&mut state, SessionEvent::ReplyStarted { session: old_handle });
    assert_eq!(result.outcome, Some(Err(SessionError::Control)));
    let turn = world.session_step(
        &mut state,
        SessionEvent::Turn(SessionTurnEvent::DispatchStarted { session: old_handle }),
    );
    assert_eq!(turn.outcome, Some(Err(SessionError::Control)));
    assert!(!state.world.pacer.session.dispatch_in_flight());
    let mut tool = world.session_step(
        &mut state,
        SessionEvent::Tool(SessionToolEvent::Started {
            session: old_handle,
            tool: ToolCallHandle {
                index: 0,
                generation: NonZeroU64::MIN,
            },
            action: SessionToolAction::End,
        }),
    );
    assert_eq!(tool.outcome, Some(Err(SessionError::Control)));
    assert!(tool.take_effect().is_none());
    let view = state.world.pacer.session.view();
    assert_eq!(view.session, Some(second.handle));
    assert!(!view.speaking);
}
