use super::{FRAME_TIME, FRAMES, ResultRow, WARM, measure, voice_reducers};
use echo_core_api::{
    BargeTrigger, CallCommand, CallEvent, CallHandle, CallPhase, CallSetupStage, Clock, Command, Commands,
    DeviceControl, DeviceEvent, DeviceState, DeviceView, Event, PacerCommand, PacerCommands, PacerEvent, PausedClock,
    PendingTrace, ReplyFrame, SeededRng, SessionCommand, SessionEvent, SessionHandle, SessionMark, SessionOutcome,
    SessionTurnEvent, Subsystem, TraceRow, UplinkEvent, UplinkReceipt, World,
};
use echo_media::PcmFrame;
use echo_sub_pacer::Pacer;
use std::{
    num::NonZeroU64,
    sync::{Mutex, MutexGuard},
    time::{Duration, UNIX_EPOCH},
};
use tokio::{runtime::Runtime, time::Instant};

type VoiceWorld =
    World<echo_sub_device::Device, voice_reducers::VoiceReducers, echo_sub_trace::Trace, PausedClock, SeededRng>;
pub(super) const RUN: SessionHandle = SessionHandle {
    index: 0,
    generation: NonZeroU64::MIN,
};

pub(super) struct Witness {
    core: Mutex<Core>,
    epoch: std::time::Instant,
}

struct Core {
    world: VoiceWorld,
    steps: usize,
    allocations: u64,
}

impl Core {
    fn count(&mut self, step: impl FnOnce(&mut VoiceWorld, &mut Commands)) -> Commands {
        let mut out = Commands::default();
        self.allocations += allocation_counter::measure(|| step(&mut self.world, &mut out)).count_total;
        self.steps += 1;
        out
    }

    fn step(&mut self, event: &mut Event<'_>) -> Commands {
        self.count(|world, out| world.step(event, out))
    }

    fn observe(&mut self, generation: u64, change: DeviceEvent) -> DeviceView {
        let Some(Command::ApplyDeviceView { view, .. }) = self.step(&mut Event::Device { generation, change }).take()
        else {
            unreachable!("missing device view")
        };
        view
    }
}

impl Witness {
    pub(super) fn new() -> Self {
        let epoch = Instant::now().into_std();
        let world = World {
            device: DeviceState::default(),
            pacer: voice_reducers::State {
                playback: echo_sub_pacer::State::default(),
                session: echo_sub_session::State::new(epoch),
                call: echo_sub_call::State::default(),
            },
            trace: echo_sub_trace::State::default(),
            clock: PausedClock::new(UNIX_EPOCH),
            rng: SeededRng::new(101),
        };
        Self {
            core: Mutex::new(Core {
                world,
                steps: 0,
                allocations: 0,
            }),
            epoch,
        }
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap()
    }

    pub(super) fn open_paths(&self) {
        let mut core = self.core();
        let response = Duration::from_secs(60);
        let mut out = core.step(&mut Event::Session(SessionEvent::Wake {
            session: RUN,
            opened: self.epoch,
            listen: None,
            max: response,
            response_due: self.epoch + response,
            reuse_call: true,
        }));
        assert!(matches!(
            out.take_session().take_effect(),
            Some(SessionCommand::StartCapture { session: RUN })
        ));
        core.step(&mut Event::Call(CallEvent::PeerAttached));
        let call = core.world.pacer.call.view().handle.unwrap();
        assert_eq!(
            core.step(&mut Event::Call(CallEvent::OpenRequested { call }))
                .take_call()
                .into_parts()
                .0,
            Some(CallCommand::OpenPeer(call))
        );
        for stage in [
            CallSetupStage::SignalingStarted,
            CallSetupStage::AnswerReceived,
            CallSetupStage::AnswerApplied,
        ] {
            core.step(&mut Event::Call(CallEvent::SetupStage { call, stage }));
        }
        core.step(&mut Event::Call(CallEvent::PeerConnected { call }));
        assert_eq!(core.world.pacer.call.view().phase, CallPhase::Connected);
        core.step(&mut Event::Call(CallEvent::Uplink {
            run: RUN,
            event: UplinkEvent::CaptureOpened,
        }));
        core.step(&mut Event::Pacer(PacerEvent::ConfigureLips {
            delay: Some(Duration::from_millis(40)),
        }));
        core.step(&mut Event::Pacer(PacerEvent::RunOpened {
            lead_ms: 0,
            adaptive: false,
        }));
        core.world.check().unwrap();
    }

    pub(super) fn reset_counts(&self) {
        let mut core = self.core();
        core.allocations = 0;
        core.steps = 0;
    }

    pub(super) fn counts(&self) -> (usize, u64) {
        let core = self.core();
        core.world.check().unwrap();
        (core.steps, core.allocations)
    }

    pub(super) fn advance(&self, duration: Duration) {
        self.core().world.clock.advance(duration);
    }

    pub(super) fn device_view(&self) -> DeviceView {
        self.core().world.device.current()
    }

    pub(super) fn step(&self, event: Event<'_>) -> Commands {
        self.core().step(&mut { event })
    }

    pub(super) fn pacer(&self, event: PacerEvent) -> PacerCommands {
        self.step(Event::Pacer(event)).take_pacer()
    }

    pub(super) fn microphone(&self, scratch: Box<PcmFrame>) -> Box<PcmFrame> {
        let mut input = Event::Pacer(PacerEvent::MicrophoneFrame(scratch));
        self.core().step(&mut input);
        let Event::Pacer(PacerEvent::MicrophoneFrame(scratch)) = input else {
            unreachable!("microphone scratch was not returned");
        };
        scratch
    }

    pub(super) fn microphone_peak(&self) -> u32 {
        self.core().world.pacer.playback.heard().level_max
    }

    pub(super) fn offer_uplink(&self, input: &PcmFrame) -> (u64, PcmFrame) {
        let (command, frame) = self
            .step(Event::Call(CallEvent::Uplink {
                run: RUN,
                event: UplinkEvent::MicFrame(input),
            }))
            .take_call()
            .into_parts();
        let Some(CallCommand::SendUplinkFrame { run: RUN, attempt }) = command else {
            unreachable!("missing uplink offer");
        };
        (attempt, frame.unwrap())
    }

    pub(super) fn accepted(&self, attempt: u64) {
        self.step(Event::Call(CallEvent::Uplink {
            run: RUN,
            event: UplinkEvent::Result {
                attempt,
                receipt: UplinkReceipt::Accepted,
            },
        }));
        assert_eq!(self.core().world.pacer.call.view().backlog_depth, 0);
    }

    pub(super) fn mute(&self) {
        let (command, frame) = self
            .step(Event::Call(CallEvent::Uplink {
                run: RUN,
                event: UplinkEvent::DropBacklog,
            }))
            .take_call()
            .into_parts();
        assert_eq!(command, Some(CallCommand::DropBacklog(RUN)));
        assert_eq!(frame, None);
        assert_eq!(self.core().world.pacer.call.view().backlog_depth, 0);
        assert!(self.device_view().privacy.mic_muted);
    }

    pub(super) fn receive(&self, input: &PcmFrame) {
        let call = self.call_handle();
        let (command, frame) = self
            .step(Event::Call(CallEvent::RemoteAudioFrame {
                call,
                generation: 0,
                frame: input,
            }))
            .take_call()
            .into_parts();
        assert_eq!(command, Some(CallCommand::DeliverRemoteAudio { call, generation: 0 }));
        assert_eq!(frame.as_ref(), Some(input));
        self.pacer(PacerEvent::PeerFrameArrived { audible: true });
    }

    fn call_handle(&self) -> CallHandle {
        self.core().world.pacer.call.view().handle.unwrap()
    }

    fn push(&self, scratch: Box<ReplyFrame>) -> Box<ReplyFrame> {
        let mut input = Event::Pacer(PacerEvent::PcmPushed(scratch));
        self.core().step(&mut input);
        let Event::Pacer(PacerEvent::PcmPushed(scratch)) = input else {
            unreachable!("playback scratch was not returned");
        };
        scratch
    }

    pub(super) fn due_frame(&self, scratch: Box<ReplyFrame>) -> (ReplyFrame, Box<ReplyFrame>) {
        let scratch = self.push(scratch);
        let mut out = self.pacer(PacerEvent::Offer { allow_fade: false });
        if out.notification().is_none() {
            let delay = {
                let core = self.core();
                Pacer::next_deadline(&core.world.pacer.playback)
                    .unwrap()
                    .checked_sub(core.world.clock.now().monotonic)
                    .unwrap()
            };
            assert_eq!(delay, FRAME_TIME);
            self.advance(delay);
            out = self.pacer(PacerEvent::Tick);
        }
        assert!(matches!(
            out.notification(),
            Some(Command::Pacer(PacerCommand::WriteFrame))
        ));
        (out.take_frame().unwrap(), scratch)
    }

    pub(super) fn played(&self) {
        assert!(matches!(
            self.pacer(PacerEvent::Played).notification(),
            Some(Command::Pacer(PacerCommand::PlayoutFinished))
        ));
        let mut out = self.step(Event::Session(SessionEvent::ReplyPlayed {
            session: RUN,
            output_stopped: true,
        }));
        assert_eq!(out.take_session().outcome, Some(Ok(SessionOutcome::Played(true))));
    }
}

impl DeviceControl for Witness {
    fn connect(&self) -> (u64, DeviceView) {
        let mut core = self.core();
        let generation = core.world.device.next_generation();
        (generation, core.observe(generation, DeviceEvent::Connected))
    }

    fn observe(&self, generation: u64, change: DeviceEvent) -> DeviceView {
        self.core().observe(generation, change)
    }
}

fn trace_tick(world: &Witness) {
    let row = PendingTrace {
        row: TraceRow {
            event: "synthetic.audio".into(),
            elapsed_ms: 0,
            mode: "sim",
            run: None,
            fields: serde_json::Map::new(),
            payload_fields: &[],
            limit: 1024,
        },
        receipt: None,
    };
    let mut input = Event::TraceRow(Some(row));
    let mut core = world.core();
    assert!(!core.step(&mut input).take_trace_wake());
    assert!(matches!(input, Event::TraceRow(None)));
    let deadline = core.world.trace_deadline().unwrap();
    let before = core.world.clock.now().monotonic;
    core.world.clock.advance(deadline.checked_sub(before).unwrap());
    assert!(core.count(World::tick_trace).take_trace_wake());
    let (through, row) = core.world.trace.next_row().unwrap();
    assert_eq!(row.row.event, "synthetic.audio");
    core.step(&mut Event::TraceFlushed { through });
}

fn pace(world: &Witness, frames: usize) {
    let mut scratch = Box::new([0; 960]);
    let mut opened = 0;
    for marker in 0..frames {
        let sample = 4_000 + u16::try_from(marker).unwrap();
        scratch.as_chunks_mut::<2>().0.fill(sample.to_le_bytes());
        let (frame, returned) = world.due_frame(scratch);
        scratch = returned;
        assert_eq!(frame, *scratch);
        world.pacer(PacerEvent::FrameHanded { screen_wanted: None });
        let out = world.core().count(World::tick_pacer).take_pacer();
        opened += usize::from(matches!(
            out.notification(),
            Some(Command::Pacer(PacerCommand::MouthChanged(true)))
        ));
        assert_eq!(world.core().world.pacer.playback.view().depth, 0);
        if marker % 5 == 4 {
            trace_tick(world);
        }
    }
    assert_eq!(opened, 1);
}

pub(super) fn pacing(runtime: &Runtime, world: &Witness) -> ResultRow {
    pace(world, WARM);
    world.pacer(PacerEvent::ReplyOpened);
    measure(runtime, world, "pacing", "probe+trace-drain", async {
        pace(world, FRAMES);
    })
}

fn barge_cycles(world: &Witness, count: usize) {
    let mut scratch = Box::new([7; 960]);
    let call = world.call_handle();
    for _ in 0..count {
        world.step(Event::Session(SessionEvent::ReplyStarted { session: RUN }));
        scratch = world.push(scratch);
        let speech = || Event::Session(SessionEvent::Turn(SessionTurnEvent::SpeechStarted { session: RUN }));
        let due = || {
            Event::Session(SessionEvent::Turn(SessionTurnEvent::BargeDue {
                session: RUN,
                finished: true,
            }))
        };
        assert_eq!(
            world.step(speech()).take_session().outcome,
            Some(Ok(SessionOutcome::Held))
        );
        assert!(matches!(
            world.step(due()).take_session().take_effect(),
            Some(SessionCommand::CutReply {
                session: RUN,
                trigger: Some(BargeTrigger::SpeechStarted)
            })
        ));
        world.pacer(PacerEvent::Cut);
        let generation = retire_reply(world, call);
        assert!(world.step(due()).take_session().take_effect().is_none());
        assert_eq!(world.core().world.pacer.playback.view().depth, 0);
        world.step(Event::Session(SessionEvent::ReplyStarted { session: RUN }));
        assert_eq!(
            world.step(speech()).take_session().outcome,
            Some(Ok(SessionOutcome::Applied))
        );
        assert!(world.step(due()).take_session().take_effect().is_none());
        world.step(Event::Session(SessionEvent::Turn(SessionTurnEvent::SpeechStopped {
            session: RUN,
        })));
        let marks = world.core().world.pacer.session.take_marks();
        assert!(matches!(marks.as_slice(), [
            SessionMark::BargeCut { turn, .. }, SessionMark::BargePassed { count: 1, .. },
            SessionMark::Latched { .. }, SessionMark::BargeRearmed { .. }
        ] if u64::from(*turn) == generation));
        world.advance(FRAME_TIME);
    }
}

fn retire_reply(world: &Witness, call: CallHandle) -> u64 {
    world.step(Event::Call(CallEvent::RetireReceive { call }));
    let (out, _) = world
        .step(Event::Call(CallEvent::BumpReceiveGeneration { call }))
        .take_call()
        .into_parts();
    let Some(CallCommand::ResetReceive { generation, .. }) = out else {
        unreachable!("missing receive reset")
    };
    assert_eq!(
        world
            .step(Event::Call(CallEvent::RemoteAudioFrame {
                call,
                generation: generation - 1,
                frame: &[0; 320],
            }))
            .take_call()
            .into_parts(),
        (None, None)
    );
    world.step(Event::Call(CallEvent::ReceiveReset { call, generation }));
    generation
}

pub(super) fn barge(runtime: &Runtime, world: &Witness) -> ResultRow {
    barge_cycles(world, WARM);
    assert!(world.device_view().capture_open);
    let row = measure(runtime, world, "barge-in", "probe+session-mark-drain", async {
        barge_cycles(world, FRAMES);
    });
    assert!(world.device_view().capture_open);
    row
}
