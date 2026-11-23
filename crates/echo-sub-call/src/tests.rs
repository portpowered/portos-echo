use super::*;

fn step(state: &mut State, event: CallEvent<'_>) -> (Option<CallCommand>, Option<echo_core_api::CallPcmFrame>) {
    let mut out = Commands::default();
    let now = Now {
        monotonic: Duration::ZERO,
        wall: std::time::UNIX_EPOCH,
    };
    LiveCall::on_event(state, &mut Event::Call(event), now, &mut out);
    out.take_call().into_parts()
}

fn attach(state: &mut State) -> CallHandle {
    assert_eq!(step(state, CallEvent::PeerAttached), (None, None));
    state.view().handle.unwrap()
}

fn apply_answer(state: &mut State, call: CallHandle) {
    for stage in [
        CallSetupStage::SignalingStarted,
        CallSetupStage::AnswerReceived,
        CallSetupStage::AnswerApplied,
    ] {
        assert_eq!(step(state, CallEvent::SetupStage { call, stage }), (None, None));
        assert_eq!(state.view().setup_stage, stage);
    }
}

#[test]
fn call_requires_signaling_and_peer_readiness_and_emits_one_close() {
    let mut state = State::default();
    let call = attach(&mut state);
    let open = CallEvent::OpenRequested { call };
    assert_eq!(step(&mut state, open), (Some(CallCommand::OpenPeer(call)), None));
    assert_eq!(step(&mut state, open), (None, None));
    step(&mut state, CallEvent::PeerConnected { call });
    assert_eq!(state.view().phase, CallPhase::Opening);
    apply_answer(&mut state, call);
    assert_eq!(state.view().phase, CallPhase::Opening);
    step(&mut state, CallEvent::PeerConnected { call });
    assert_eq!(state.view().phase, CallPhase::Connected);
    assert_eq!(state.view().setup_stage, CallSetupStage::MediaReady);

    assert_eq!(
        step(&mut state, CallEvent::PeerFailed { call }),
        (Some(CallCommand::ClosePeer(call)), None)
    );
    assert_eq!(step(&mut state, CallEvent::CloseRequested { call }), (None, None));
    step(&mut state, CallEvent::PeerClosed { call });
    assert_eq!(state.view().phase, CallPhase::Closed);
    assert_eq!(step(&mut state, open), (None, None));

    let fresh = attach(&mut state);
    assert_ne!(call, fresh);
    step(&mut state, CallEvent::PeerClosed { call });
    assert_eq!(state.view().phase, CallPhase::Fresh);
    assert_eq!(
        step(&mut state, CallEvent::CloseRequested { call: fresh }),
        (Some(CallCommand::ClosePeer(fresh)), None)
    );
    assert_eq!(step(&mut state, CallEvent::OpenRequested { call: fresh }), (None, None));
}

#[test]
fn receive_reset_fences_stale_generations_and_delivers_exact_pcm() {
    let mut state = State::default();
    let old = attach(&mut state);
    step(&mut state, CallEvent::CloseRequested { call: old });
    step(&mut state, CallEvent::PeerClosed { call: old });
    let call = attach(&mut state);
    step(&mut state, CallEvent::OpenRequested { call });
    apply_answer(&mut state, call);
    step(&mut state, CallEvent::PeerConnected { call });

    let frame = std::array::from_fn(|sample| i16::try_from(sample).unwrap() - 160);
    let audio = |generation| CallEvent::RemoteAudioFrame {
        call,
        generation,
        frame: &frame,
    };
    step(&mut state, CallEvent::RetireReceive { call });
    assert_eq!(state.view().receive, CallReceive::Retired);
    // The run adapter still observes late retired audio for drop accounting.
    assert_eq!(
        step(&mut state, audio(0)),
        (
            Some(CallCommand::DeliverRemoteAudio { call, generation: 0 }),
            Some(frame)
        )
    );
    let bump = CallEvent::BumpReceiveGeneration { call };
    assert_eq!(
        step(&mut state, bump),
        (Some(CallCommand::ResetReceive { call, generation: 1 }), None)
    );
    assert_eq!(step(&mut state, bump), (None, None));
    step(
        &mut state,
        CallEvent::ReceiveReset {
            call: old,
            generation: 1,
        },
    );
    step(&mut state, CallEvent::ReceiveReset { call, generation: 0 });
    assert_eq!(state.view().receive, CallReceive::Resetting);
    assert_eq!(step(&mut state, audio(1)), (None, None));

    step(&mut state, CallEvent::ReceiveReset { call, generation: 1 });
    assert_eq!(state.view().receive, CallReceive::Current);
    assert_eq!(step(&mut state, audio(0)), (None, None));
    assert_eq!(
        step(&mut state, audio(1)),
        (
            Some(CallCommand::DeliverRemoteAudio { call, generation: 1 }),
            Some(frame)
        )
    );
}

#[test]
fn call_world_seeded_invariant() {
    let mut state = State::default();
    let call = attach(&mut state);
    step(&mut state, CallEvent::CloseRequested { call });
    step(&mut state, CallEvent::PeerClosed { call });
    assert_eq!(LiveCall::check(&state), Ok(()));

    state.view.receive = CallReceive::Resetting;
    let broken = LiveCall::check(&state).unwrap_err();
    assert_eq!(
        broken,
        Broken {
            field: "call.receive_state",
            expected: "no reset after close",
            actual: "reset pending",
        }
    );
    state.view.receive = CallReceive::Current;
    assert_eq!(LiveCall::check(&state), Ok(()));
}
