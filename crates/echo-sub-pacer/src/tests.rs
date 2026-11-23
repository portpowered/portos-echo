use super::*;
use echo_core_api::{Clock, Command, PacerCommand, PausedClock, SeededRng, World};
use std::{io::Write, time::UNIX_EPOCH};

struct Empty;

impl Subsystem for Empty {
    type State = ();
    const INPUTS: &'static [EventKind] = &[];
    const OUTPUTS: &'static [CommandKind] = &[];

    fn on_event((): &mut (), _: &mut Event, _: Now, _: &mut Commands) {}
    fn next_deadline((): &()) -> Option<Duration> {
        None
    }
    fn check((): &()) -> Result<(), Broken> {
        Ok(())
    }
}

type TestWorld = World<Empty, Pacer, Empty, PausedClock, SeededRng>;

fn world() -> TestWorld {
    World {
        device: (),
        pacer: State::default(),
        trace: (),
        clock: PausedClock::new(UNIX_EPOCH),
        rng: SeededRng::new(101),
    }
}

fn step(world: &mut TestWorld, event: PacerEvent) -> PacerCommands {
    let mut commands = Commands::default();
    world.step(&mut Event::Pacer(event), &mut commands);
    commands.take_pacer()
}

fn open(world: &mut TestWorld, lead_ms: u16, adaptive: bool) {
    step(world, PacerEvent::RunOpened { lead_ms, adaptive });
}

fn frame(marker: u16) -> ReplyFrame {
    let mut frame = [0; 960];
    frame.as_chunks_mut::<2>().0.fill(marker.to_le_bytes());
    frame
}

fn push(world: &mut TestWorld, scratch: Box<ReplyFrame>) -> Box<ReplyFrame> {
    let mut event = Event::Pacer(PacerEvent::PcmPushed(scratch));
    world.step(&mut event, &mut Commands::default());
    let scratch = match event {
        Event::Pacer(PacerEvent::PcmPushed(scratch)) => Some(scratch),
        _ => None,
    };
    scratch.unwrap()
}

fn offer(world: &mut TestWorld) -> PacerCommands {
    step(world, PacerEvent::Offer { allow_fade: false })
}

fn handed(world: &mut TestWorld) {
    step(world, PacerEvent::FrameHanded { screen_wanted: None });
}

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test]
fn pacer_ring_300_keeps_capacity_and_matches_old_release_clock() {
    let mut world = world();
    let storage = world.pacer.queue.storage();
    open(&mut world, 200, false);
    let mut scratch = check_overflow(&mut world);
    step(&mut world, PacerEvent::ReplyOpened);
    assert_eq!(world.pacer.queue.storage(), storage);
    assert_eq!(world.pacer.dropped(), 0);
    let start = world.clock.now().monotonic;
    for marker in 0..300 {
        *scratch = frame(marker);
        scratch = push(&mut world, scratch);
        assert_eq!(release(&mut world), frame(marker));
        // The old ClockPacer sent frame k at start + 20 ms * (k + 1) - lead,
        // using the same opening credit for every frame whose offset is zero.
        let expected = ms((20 * (u64::from(marker) + 1)).saturating_sub(200));
        let elapsed = world.clock.now().monotonic.checked_sub(start).unwrap();
        assert_eq!(elapsed, expected, "frame {marker}");
        assert_eq!(world.pacer.queue.storage(), storage);
    }
    step(&mut world, PacerEvent::RunClosed);
    assert_eq!(world.pacer.queue.storage(), storage);
    assert!(world.pacer.quiescent());
    writeln!(
        std::io::stdout().lock(),
        "pacer ring capacity=128 pushes=300 evictions=172 retained=128 release_offsets=300"
    )
    .unwrap();
}

fn check_overflow(world: &mut TestWorld) -> Box<ReplyFrame> {
    let storage = world.pacer.queue.storage();
    let mut scratch = Box::new([0; 960]);
    let scratch_ptr = std::ptr::from_ref(scratch.as_ref());
    for marker in 0..300 {
        *scratch = frame(marker);
        scratch = push(world, scratch);
        assert_eq!(world.pacer.queue.storage(), storage);
        assert_eq!(std::ptr::from_ref(scratch.as_ref()), scratch_ptr);
    }
    assert_eq!(
        (storage.0, world.pacer.view().depth, world.pacer.dropped()),
        (128, 128, 172)
    );
    step(world, PacerEvent::InputFinished);
    for marker in 172..300 {
        assert_eq!(release(world), frame(marker));
        assert_eq!(world.pacer.queue.storage(), storage);
    }
    assert_eq!(world.pacer.view().depth, 0);
    scratch
}

fn release(world: &mut TestWorld) -> ReplyFrame {
    let mut commands = offer(world);
    if commands.notification().is_none() {
        let due = Pacer::next_deadline(&world.pacer).unwrap();
        let delay = due.checked_sub(world.clock.now().monotonic).unwrap();
        world.clock.advance(delay);
        commands = step(world, PacerEvent::Tick);
    }
    assert!(matches!(
        commands.notification(),
        Some(Command::Pacer(PacerCommand::WriteFrame))
    ));
    let frame = commands.take_frame().unwrap();
    handed(world);
    frame
}

#[test]
fn seeded_pacer_world_diff() {
    let mut world = world();
    world.check().unwrap();
    world.pacer.queue.corrupt_depth(REPLY_CAPACITY + 1);
    let diff = world.check().unwrap_err();
    let rendered = format!("{}: expected {}, actual {}", diff.field, diff.expected, diff.actual);
    assert_eq!(rendered, "pacer.reply_depth: expected 0..=128, actual out of range");
    writeln!(std::io::stdout().lock(), "World diff: {rendered}").unwrap();
    world.pacer.queue.corrupt_depth(0);
    world.check().unwrap();
}

#[test]
fn pacer_cut_discards_a_staged_and_a_queued_frame_then_accepts_receipt() {
    let mut world = world();
    open(&mut world, 200, false);
    push(&mut world, Box::new(frame(1)));
    push(&mut world, Box::new(frame(2)));
    assert_eq!(offer(&mut world).take_frame(), Some(frame(1)));
    assert!(step(&mut world, PacerEvent::Played).notification().is_none());
    assert_eq!(world.pacer.view().depth, 1);
    step(&mut world, PacerEvent::Cut);
    assert_eq!(world.pacer.view(), PacerView::default());
    assert!(offer(&mut world).take_frame().is_none());
    assert!(step(&mut world, PacerEvent::Tick).take_frame().is_none());
    assert!(matches!(
        step(&mut world, PacerEvent::Played).notification(),
        Some(Command::Pacer(PacerCommand::PlayoutFinished))
    ));
    step(&mut world, PacerEvent::ReplyOpened);
    push(&mut world, Box::new(frame(3)));
    assert_eq!(release(&mut world), frame(3));
    assert_eq!(world.pacer.view().last_frame_at, Some(Duration::ZERO));
}

#[test]
fn pacer_playout_prime_survives_reoffers_and_restarts_without_waiting_when_full() {
    let mut world = world();
    open(&mut world, 60, true);
    push(&mut world, Box::new(frame(1)));
    push(&mut world, Box::new(frame(2)));
    for _ in 0..3 {
        assert!(offer(&mut world).take_frame().is_none());
        assert_eq!(Pacer::next_deadline(&world.pacer), Some(ms(60)));
        world.clock.advance(ms(10));
    }
    assert_eq!(world.pacer.view().depth, 2);
    assert_eq!(release(&mut world), frame(1));
    assert_eq!(world.clock.now().monotonic, ms(60));
    assert_eq!(release(&mut world), frame(2));
    assert_eq!(world.clock.now().monotonic, ms(60));

    // After the speaker runs dry, a full 60 ms cushion restarts immediately.
    world.clock.advance(ms(1000));
    for marker in 3..6 {
        push(&mut world, Box::new(frame(marker)));
    }
    for marker in 3..6 {
        assert_eq!(release(&mut world), frame(marker));
        assert_eq!(world.clock.now().monotonic, ms(1060));
    }

    // EOF releases an incomplete cushion on a later dry restart.
    world.clock.advance(ms(1000));
    push(&mut world, Box::new(frame(6)));
    step(&mut world, PacerEvent::InputFinished);
    assert_eq!(release(&mut world), frame(6));
    assert_eq!(world.clock.now().monotonic, ms(2060));
    assert!(matches!(
        step(&mut world, PacerEvent::Played).notification(),
        Some(Command::Pacer(PacerCommand::PlayoutFinished))
    ));

    // A smooth arrival keeps the lead-bounded floor. 160 ms of lateness
    // plus the 40 ms margin then lifts the target to 200 ms.
    step(&mut world, PacerEvent::PeerFrameArrived { audible: false });
    assert_eq!(world.pacer.target(), ms(60));
    world.clock.advance(ms(180));
    step(&mut world, PacerEvent::PeerFrameArrived { audible: false });
    assert_eq!(world.pacer.target(), ms(200));
}

#[test]
fn pacer_playout_skips_one_silent_frame_to_reduce_excess_cushion() {
    let mut world = world();
    open(&mut world, 200, true);
    for _ in 0..5 {
        push(&mut world, Box::new(frame(8000)));
    }
    for _ in 0..5 {
        assert_eq!(release(&mut world), frame(8000));
    }
    world.clock.advance(ms(120));
    push(&mut world, Box::new(frame(0)));
    for marker in 8001..=8007 {
        push(&mut world, Box::new(frame(marker)));
    }
    assert_eq!(release(&mut world), frame(8001));
    assert_eq!(world.pacer.view().depth, 6);
    let playout = world.pacer.playout();
    assert_eq!((playout.reprimes, playout.drained), (1, 1));
    step(&mut world, PacerEvent::ReplySettled);
    assert_eq!(world.pacer.playout(), Playout::default());
}
