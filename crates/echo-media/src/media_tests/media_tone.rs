use std::io::Write;
// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Modified by Port OS: synthetic PCM journey and frequency/RMS oracle.
use crate::{DropCounts, MediaPeer, PcmFrame, Peer, media_audio::pcm16};
use std::{f64::consts::TAU, time::Duration};

fn frame(index: u32, hz: u32) -> PcmFrame {
    std::array::from_fn(|n| {
        let sample = index * 320 + u32::try_from(n).unwrap();
        pcm16(16384.0 * (TAU * f64::from(hz) * f64::from(sample) / 16_000.0).sin())
    })
}

fn power(samples: &[i16], hz: u32) -> f64 {
    // Goertzel scans the entire Nyquist range; refine the strongest coarse bin.
    let coefficient = 2.0 * (TAU * f64::from(hz) / 16_000.0).cos();
    let (mut previous, mut older) = (0.0, 0.0);
    for sample in samples {
        let next = f64::from(*sample) + coefficient * previous - older;
        older = previous;
        previous = next;
    }
    previous * previous + older * older - coefficient * previous * older
}

fn dominant(samples: &[i16]) -> u32 {
    let coarse = (50..8000)
        .step_by(50)
        .max_by(|a, b| power(samples, *a).total_cmp(&power(samples, *b)))
        .unwrap();
    (coarse.saturating_sub(49).max(1)..=(coarse + 49).min(7999))
        .max_by(|a, b| power(samples, *a).total_cmp(&power(samples, *b)))
        .unwrap()
}

fn rms(samples: &[i16]) -> f64 {
    (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / f64::from(u32::try_from(samples.len()).unwrap()))
        .sqrt()
}

pub(super) struct Clock {
    ready: tokio::sync::Barrier,
    next: tokio::sync::Barrier,
}
impl Clock {
    pub(super) fn new(producers: usize) -> Self {
        Self {
            ready: tokio::sync::Barrier::new(producers + 1),
            next: tokio::sync::Barrier::new(producers + 1),
        }
    }
    pub(super) async fn drive(&self) {
        for _ in 0..100 {
            self.ready.wait().await;
            tokio::time::advance(Duration::from_millis(20)).await;
            self.next.wait().await;
        }
    }
}

pub(super) struct VirtualTime(tokio::task::JoinHandle<()>);
impl VirtualTime {
    pub(super) fn start() -> Self {
        tokio::time::pause();
        // Keep automatic clock jumps from racing real OS I/O; only Clock.drive
        // advances virtual source time, while readiness drives transport work.
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}
impl Drop for VirtualTime {
    fn drop(&mut self) {
        self.0.abort();
        tokio::time::resume();
    }
}

pub(super) async fn journey(sender: &Peer, receiver: &mut Peer, hz: u32, clock: &Clock) {
    let mut output = Vec::new();
    let mut sent = sender.shared.sent.subscribe();
    let mut ingress = receiver.shared.received.subscribe();
    let sent_before = sent.borrow().0;
    let packets_before = sent.borrow().1;
    let rx_before = ingress.borrow().0;
    let frames_before = ingress.borrow().1;
    for index in 0..100 {
        clock.ready.wait().await;
        clock.next.wait().await;
        sender.send(frame(index, hz)).unwrap();
        let packets = sent.wait_for(|p| p.0 > sent_before + index).await.unwrap().1 - packets_before;
        let frames = ingress.wait_for(|p| p.0 >= rx_before + packets).await.unwrap().1 - frames_before;
        while output.len() < usize::try_from(frames).unwrap() * 320 {
            output.extend(receiver.recv().await.unwrap().unwrap());
        }
    }
    let input: Vec<_> = (0..100).flat_map(|n| frame(n, hz)).collect();
    // Fixed 250..500 ms window is after the declared codec/filter delay,
    // and exists even when the seeded conversion bug produces only one third.
    let window = output.get(4000..8000).unwrap_or(&[]);
    let dominant_hz = dominant(window);
    let input_rms = rms(&input[4000..8000]);
    let output_rms = rms(window);
    let rms_delta_db = 20.0 * (output_rms / input_rms).log10();
    writeln!(std::io::stdout(),
        "input_samples=32000 tone_ms=2000 dominant_hz={dominant_hz} input_rms={input_rms:.2} output_rms={output_rms:.2} rms_delta_db={rms_delta_db:.3} send_drops={:?} recv_drops={:?}",
        sender.dropped_frames(),
        receiver.dropped_frames()
    ).unwrap();
    assert!(
        (hz - 20..=hz + 20).contains(&dominant_hz),
        "dominant-frequency assertion: {dominant_hz} Hz, expected {hz} +/-20"
    );
    assert!(output.len() >= 30_400);
    assert!(rms_delta_db.abs() <= 3.0);
    assert_eq!(sender.dropped_frames(), DropCounts::default());
    assert_eq!(receiver.dropped_frames(), DropCounts::default());
}

#[tokio::test]
async fn t3_peer_tone_both_directions() {
    let (mut a, mut b) = super::pair().await;
    let virtual_time = VirtualTime::start();
    let clock = Clock::new(1);
    tokio::join!(clock.drive(), journey(&a, &mut b, 1000, &clock));
    tokio::join!(clock.drive(), journey(&b, &mut a, 1000, &clock));
    drop(virtual_time);
    a.close().await.unwrap();
    b.close().await.unwrap();
}
