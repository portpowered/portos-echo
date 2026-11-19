use super::*;

fn frame(level: i16) -> Vec<u8> {
    level.to_le_bytes().repeat(480)
}

/// A bounded media sender that takes `room` more frames, then refuses.
#[derive(Default)]
struct Narrow {
    room: std::cell::Cell<usize>,
    sent: std::cell::RefCell<Vec<i16>>,
}

impl VoicePort for Narrow {
    fn send(&self, _frame: PcmFrame) -> Result<(), Error> {
        // The evicting sender is never an acceptable path for speech.
        Err(Error::Audio)
    }
    fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
        if self.room.get() == 0 {
            return Ok(false);
        }
        self.room.set(self.room.get() - 1);
        self.sent.borrow_mut().push(frame[0]);
        Ok(true)
    }
    async fn next(&mut self) -> Result<crate::bridge_codex::MediaEvent, Error> {
        std::future::pending().await
    }
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, Error>> {
        std::future::ready(Ok(None))
    }
}

#[test]
fn setup_burst_waits_for_the_sender_in_order_and_mute_withholds_the_rest() {
    let mut source = Source::new(200, false).unwrap();
    let port = Narrow::default();
    port.room.set(2);
    for marker in 1..=5 {
        source.forward(&[marker; 320], &port).unwrap();
    }
    assert_eq!(*port.sent.borrow(), [1, 2]);
    assert!(source.uplink_pending());
    assert_eq!(source.stats.microphone_frames, 5);
    port.room.set(2);
    source.flush_uplink(&port).unwrap();
    assert_eq!(*port.sent.borrow(), [1, 2, 3, 4]);
    // Room for one: waiting speech goes first, the new frame queues behind it.
    port.room.set(1);
    source.forward(&[6; 320], &port).unwrap();
    assert_eq!(*port.sent.borrow(), [1, 2, 3, 4, 5]);
    assert!(source.uplink_pending());
    source.mute_uplink();
    port.room.set(5);
    source.flush_uplink(&port).unwrap();
    assert_eq!(*port.sent.borrow(), [1, 2, 3, 4, 5]);
    assert!(!source.uplink_pending());
    assert_eq!(source.stats.microphone_frames, 6);
}

#[test]
fn stalled_sender_backlog_is_bounded_and_keeps_the_newest_speech() {
    let mut source = Source::new(200, false).unwrap();
    let port = Narrow::default();
    for marker in 0..=UPLINK_BACKLOG {
        source.forward(&[i16::try_from(marker).unwrap(); 320], &port).unwrap();
    }
    port.room.set(usize::MAX);
    source.flush_uplink(&port).unwrap();
    let sent = port.sent.borrow();
    assert_eq!(sent.len(), UPLINK_BACKLOG);
    assert_eq!((sent[0], sent[UPLINK_BACKLOG - 1]), (1, 1500));
    assert_eq!(source.stats.uplink_dropped, 1);
}

#[tokio::test(start_paused = true)]
async fn mouth_opens_and_closes_on_the_echo_playout_clock_not_on_writes() {
    let mut mouth = Mouth::default();
    let t0 = Instant::now();
    // Silence first, then a lead burst of 5 audible frames written at t0.
    mouth.wrote(&frame(0), t0);
    for _ in 0..5 {
        mouth.wrote(&frame(QUIET.cast_signed() + 1), t0);
    }
    assert_eq!(mouth.due(), Some(t0 + FRAME + OUTPUT_DELAY));
    assert_eq!(mouth.poll(t0 + OUTPUT_DELAY), None);
    assert_eq!(mouth.poll(t0 + FRAME + OUTPUT_DELAY), Some(true));
    // Five audible frames after the leading silence finish at 120 ms.
    assert_eq!(mouth.due(), Some(t0 + FRAME * 6 + OUTPUT_DELAY));
    // Quiet frames keep the clock running but never reopen or extend it.
    mouth.wrote(&frame(-QUIET.cast_signed()), t0 + FRAME * 3);
    assert_eq!(mouth.poll(t0 + FRAME * 6 + OUTPUT_DELAY), Some(false));
    assert_eq!(mouth.due(), None);
}

#[tokio::test(start_paused = true)]
async fn mouth_restarts_the_clock_after_the_echo_ran_dry() {
    let mut mouth = Mouth::default();
    let t0 = Instant::now();
    mouth.wrote(&frame(1000), t0);
    assert_eq!(mouth.poll(t0 + OUTPUT_DELAY), Some(true));
    assert_eq!(mouth.poll(t0 + FRAME + OUTPUT_DELAY), Some(false));
    let later = t0 + Duration::from_secs(3);
    mouth.wrote(&frame(-1000), later);
    assert_eq!(mouth.due(), Some(later + OUTPUT_DELAY));
    assert_eq!(mouth.poll(later + OUTPUT_DELAY), Some(true));
}

#[tokio::test(start_paused = true)]
async fn mouth_quiet_thresholds_never_schedule_a_transition() {
    let mut mouth = Mouth::default();
    let t0 = Instant::now();
    for level in [0, 64, -64] {
        mouth.wrote(&frame(level), t0);
    }
    mouth.wrote(&[], t0);
    assert_eq!(mouth.due(), None);
    assert_eq!(mouth.poll(t0 + Duration::from_secs(1)), None);
}

#[tokio::test(start_paused = true)]
async fn mouth_preserves_quiet_gaps_even_when_all_audio_is_written_ahead() {
    let mut mouth = Mouth::default();
    let t0 = Instant::now();
    for level in [65, 0, -65] {
        mouth.wrote(&frame(level), t0);
    }
    for (offset, open) in [(0, true), (1, false), (2, true), (3, false)] {
        let due = t0 + OUTPUT_DELAY + FRAME * offset;
        assert_eq!(mouth.poll(due - Duration::from_nanos(1)), None);
        assert_eq!(mouth.poll(due), Some(open));
    }
    assert_eq!(mouth.due(), None);
}
