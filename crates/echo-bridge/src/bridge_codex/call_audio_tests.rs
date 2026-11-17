//! T1 PCM/pacing component: no sockets, processes or provider substitutes.
use super::*;
use std::{future::Future, task::Poll, time::Duration};
use tokio::time::Instant;

fn bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[test]
fn capture_preserves_irregular_pcm_and_reports_unpadded_tail() {
    let format = crate::bridge_audio::format(16_000);
    let mut capture = CaptureAudio::new(format).unwrap();
    let input: Vec<i16> = (0..1003).collect();
    let mut frames = Vec::new();
    for chunk in input.chunks(137) {
        frames.extend(capture.process(&format, &bytes(chunk)).unwrap());
    }
    frames.extend(capture.finish().unwrap());
    assert_eq!(frames.len(), 3);
    assert_eq!(frames.concat(), input[..960]);
    assert_eq!(capture.tail_samples(), 43);
    assert_eq!(capture.finish().unwrap(), Vec::<PcmFrame>::new());
    assert_eq!(capture.process(&format, &[]), Err(Error::Audio));
}

#[test]
fn capture_validation_consumes_nothing_and_new_owner_discards_old_tail() {
    let format = PcmFormat {
        channels: 2,
        ..crate::bridge_audio::format(16_000)
    };
    let mut capture = CaptureAudio::new(format).unwrap();
    assert_eq!(capture.process(&format, &[1, 0]), Err(Error::Audio));
    assert_eq!(capture.process(&format, &[0; 8196]), Err(Error::Audio));
    assert_eq!(
        capture.process(&crate::bridge_audio::format(16_000), &[]),
        Err(Error::Audio)
    );
    let stereo = [i16::MAX, i16::MIN].repeat(320);
    assert_eq!(capture.process(&format, &bytes(&stereo)).unwrap(), vec![[0; 320]]);
    assert_eq!(
        capture.process(&format, &bytes(&[6000; 100])).unwrap(),
        Vec::<PcmFrame>::new()
    );
    drop(capture);
    let mut capture = CaptureAudio::new(format).unwrap();
    assert_eq!(capture.process(&format, &bytes(&[0; 640])).unwrap(), vec![[0; 320]]);
    assert_eq!(capture.tail_samples(), 0);
}

#[test]
fn capture_supported_rates_and_stereo_keep_exact_duration() {
    for rate in [16_000, 22_050, 24_000, 48_000] {
        for channels in [1, 2] {
            let format = PcmFormat {
                rate,
                width: 2,
                channels,
            };
            let mut capture = CaptureAudio::new(format).unwrap();
            let input = vec![0; usize::try_from(rate / 10).unwrap() * usize::from(channels)];
            let mut frames = Vec::new();
            for chunk in input.chunks(139 * usize::from(channels)) {
                frames.extend(capture.process(&format, &bytes(chunk)).unwrap());
            }
            frames.extend(capture.finish().unwrap());
            assert_eq!(frames, vec![[0; 320]; 5]);
            assert_eq!(capture.tail_samples(), 0);
        }
    }
    for format in [
        PcmFormat {
            rate: 8000,
            width: 2,
            channels: 1,
        },
        PcmFormat {
            rate: 16_000,
            width: 1,
            channels: 1,
        },
        PcmFormat {
            rate: 16_000,
            width: 2,
            channels: 0,
        },
    ] {
        assert!(matches!(CaptureAudio::new(format), Err(Error::Audio)));
    }
}

#[tokio::test(start_paused = true)]
async fn reply_flush_retains_exact_pcm_duration_and_never_implies_played() {
    let mut reply = ReplyAudio::new(200).unwrap();
    assert!(!reply.drained());
    for _ in 0..4 {
        reply.push(&[0; 320]).unwrap();
    }
    assert!(!reply.drained());
    reply.finish().unwrap();
    reply.finish().unwrap();
    for _ in 0..4 {
        assert!(!reply.drained());
        assert_eq!(reply.next_frame().await.unwrap(), vec![0; 960]);
    }
    assert!(reply.drained());
    assert_eq!(reply.dropped_frames(), 0);
    assert_eq!(reply.next_frame().await, None);
    assert_eq!(reply.push(&[0; 320]), Err(Error::Audio));
    // Drained only counts handoff; no fake Wyoming played flag is introduced.
}

#[tokio::test(start_paused = true)]
async fn reply_paces_ten_frames_then_twenty_ms_and_does_not_catch_up() {
    let mut reply = ReplyAudio::new(200).unwrap();
    for _ in 0..14 {
        reply.push(&[0; 320]).unwrap();
    }
    reply.finish().unwrap();
    let start = Instant::now();
    for _ in 0..10 {
        assert_eq!(reply.next_frame().await.unwrap().len(), 960);
    }
    assert_eq!(Instant::now(), start);
    assert_eq!(reply.next_frame().await.unwrap().len(), 960);
    assert_eq!(Instant::now() - start, Duration::from_millis(20));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(reply.next_frame().await.unwrap().len(), 960);
    let late = Instant::now();
    assert_eq!(reply.next_frame().await.unwrap().len(), 960);
    assert_eq!(Instant::now() - late, Duration::from_millis(20));
}

#[tokio::test(start_paused = true)]
async fn reply_cancelled_wait_keeps_frame_and_underflow_never_renews_credit() {
    let mut reply = ReplyAudio::new(20).unwrap();
    reply.push(&[0; 320]).unwrap();
    reply.push(&[0; 320]).unwrap();
    reply.push(&[0; 320]).unwrap();
    assert_eq!(reply.next_frame().await.unwrap(), vec![0; 960]);
    let mut pending = Box::pin(reply.next_frame());
    std::future::poll_fn(|cx| {
        assert!(pending.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(pending);
    assert!(!reply.drained());
    assert_eq!(reply.next_frame().await.unwrap(), vec![0; 960]);
    assert_eq!(reply.next_frame().await, None);
    tokio::time::advance(Duration::from_secs(1)).await;
    reply.push(&[0; 320]).unwrap();
    reply.finish().unwrap();
    assert_eq!(reply.next_frame().await.unwrap(), vec![0; 960]);
    let late = Instant::now();
    assert_eq!(reply.next_frame().await.unwrap(), vec![0; 960]);
    assert_eq!(Instant::now() - late, Duration::from_millis(20));
    assert!(reply.drained());
}

#[tokio::test(start_paused = true)]
async fn reply_overload_evicts_exact_oldest_frames_and_preserves_retained_order() {
    let mut overloaded = ReplyAudio::new(200).unwrap();
    let mut reference = ReplyAudio::new(200).unwrap();
    let mut expected = Vec::new();
    for index in 0..140_i16 {
        let input = [index * 100; 320];
        overloaded.push(&input).unwrap();
        reference.push(&input).unwrap();
        while let Some(frame) = reference.next_frame().await {
            expected.push(frame);
        }
    }
    overloaded.finish().unwrap();
    reference.finish().unwrap();
    while let Some(frame) = reference.next_frame().await {
        expected.push(frame);
    }
    assert_eq!(expected.len(), 140);
    assert_eq!(overloaded.dropped_frames(), 12);
    let mut actual = Vec::new();
    while let Some(frame) = overloaded.next_frame().await {
        actual.push(frame);
    }
    assert_eq!(actual, expected[12..]);
    assert!(overloaded.drained());
    assert_eq!(reference.dropped_frames(), 0);
}

#[tokio::test(start_paused = true)]
async fn zero_reply_lead_waits_before_every_frame() {
    let mut reply = ReplyAudio::new(0).unwrap();
    reply.push(&[0; 320]).unwrap();
    reply.push(&[0; 320]).unwrap();
    reply.finish().unwrap();
    let start = Instant::now();
    assert_eq!(reply.next_frame().await.unwrap(), vec![0; 960]);
    assert_eq!(Instant::now() - start, Duration::from_millis(20));
    assert_eq!(reply.next_frame().await.unwrap(), vec![0; 960]);
    assert_eq!(Instant::now() - start, Duration::from_millis(40));
    assert!(reply.drained());
}

#[tokio::test(start_paused = true)]
async fn empty_reply_and_new_owner_have_no_old_pcm_or_lead_state() {
    let mut reply = ReplyAudio::new(0).unwrap();
    reply.push(&[12_000; 320]).unwrap();
    drop(reply);
    let mut empty = ReplyAudio::new(200).unwrap();
    empty.finish().unwrap();
    assert_eq!(empty.next_frame().await, None);
    assert!(empty.drained());
    let mut fresh = ReplyAudio::new(200).unwrap();
    fresh.push(&[0; 320]).unwrap();
    fresh.finish().unwrap();
    let start = Instant::now();
    assert_eq!(fresh.next_frame().await.unwrap(), vec![0; 960]);
    assert_eq!(Instant::now(), start);
    assert!(fresh.drained());
}
