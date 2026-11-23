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
        frames.extend_from_slice(capture.process(&format, &bytes(chunk)).unwrap());
    }
    frames.extend_from_slice(capture.finish().unwrap());
    assert_eq!(frames.len(), 3);
    assert_eq!(frames.concat(), input[..960]);
    assert_eq!(capture.tail_samples(), 43);
    assert_eq!(capture.finish().unwrap().len(), 0);
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
    assert_eq!(capture.process(&format, &bytes(&stereo)).unwrap(), &[[0; 320]]);
    assert_eq!(capture.process(&format, &bytes(&[6000; 100])).unwrap().len(), 0);
    drop(capture);
    let mut capture = CaptureAudio::new(format).unwrap();
    assert_eq!(capture.process(&format, &bytes(&[0; 640])).unwrap(), &[[0; 320]]);
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
                frames.extend_from_slice(capture.process(&format, &bytes(chunk)).unwrap());
            }
            frames.extend_from_slice(capture.finish().unwrap());
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

#[test]
fn capture_maximum_chunks_and_finish_reuse_scratch_at_every_supported_format() {
    for rate in [16_000, 22_050, 24_000, 48_000] {
        for channels in [1, 2] {
            let format = PcmFormat {
                rate,
                width: 2,
                channels,
            };
            let mut capture = CaptureAudio::new(format).unwrap();
            let sample_base = capture.samples.as_ptr();
            let frame_base = capture.frames.as_ptr();
            let capacity = capture.frames.capacity();
            let chunks = [CAPTURE_BYTES - 4, CAPTURE_BYTES, CAPTURE_BYTES, CAPTURE_BYTES];
            let mut completed_samples = 0;
            for size in chunks {
                let frames = capture.process(&format, &[0; CAPTURE_BYTES][..size]).unwrap();
                completed_samples += frames.len() * 320;
                assert_eq!(frames.as_ptr(), frame_base);
                assert!(frames.iter().all(|frame| frame == &[0; 320]));
            }
            completed_samples += capture.finish().unwrap().len() * 320;
            let expected =
                chunks.iter().sum::<usize>() / (2 * usize::from(channels)) * 16_000 / usize::try_from(rate).unwrap();
            assert_eq!(
                (completed_samples, capture.tail_samples()),
                (expected / 320 * 320, expected % 320)
            );
            assert_eq!(capture.samples.as_ptr(), sample_base);
            assert_eq!(capture.frames.as_ptr(), frame_base);
            assert_eq!(capture.frames.capacity(), capacity);
        }
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
async fn reply_paces_ten_frames_then_twenty_ms_and_restarts_the_lead_after_the_device_ran_dry() {
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
    let late = Instant::now();
    for _ in 0..3 {
        assert_eq!(reply.next_frame().await.unwrap().len(), 960);
    }
    assert_eq!(Instant::now(), late);
    assert!(reply.drained());
}

#[tokio::test(start_paused = true)]
async fn real_time_reply_primes_the_echo_once_then_goes_out_on_arrival() {
    let mut reply = ReplyAudio::new(200).unwrap();
    let start = Instant::now();
    let mut sent = Vec::new();
    for _ in 0..60 {
        reply.push(&[0; 320]).unwrap();
        // The run loop drops a waiting send whenever the next frame arrives.
        while reply.has_frames() {
            let Ok(frame) = tokio::time::timeout(Duration::ZERO, reply.next_frame()).await else {
                break;
            };
            frame.unwrap();
            sent.push(Instant::now() - start);
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    // The converter holds one frame of filter delay; 59 left for playback.
    assert_eq!(sent.len(), 59);
    // The Echo starts with 100 ms of audio at once, not with a single frame.
    assert_eq!(sent[..5], [Duration::from_millis(100); 5]);
    // After the prime each frame leaves as it arrives: the delay never grows.
    for (index, at) in sent.iter().enumerate().skip(5) {
        assert_eq!(*at, Duration::from_millis(20 * (index as u64 + 1)));
    }
}

#[tokio::test(start_paused = true)]
async fn a_burst_after_a_stall_is_sent_at_most_the_lead_ahead_of_the_clock() {
    let mut reply = ReplyAudio::new(200).unwrap();
    for _ in 0..31 {
        reply.push(&[0; 320]).unwrap();
    }
    let start = Instant::now();
    let mut sent = Vec::new();
    while reply.has_frames() {
        reply.next_frame().await.unwrap();
        sent.push(Instant::now() - start);
    }
    assert_eq!(sent.len(), 30);
    // Ten frames (the 200 ms lead) go at once, then one per 20 ms of clock.
    assert_eq!(sent[9], Duration::ZERO);
    for (index, at) in sent.iter().enumerate().skip(10) {
        assert_eq!(*at, Duration::from_millis(20 * (index as u64 - 9)));
    }
}

#[tokio::test(start_paused = true)]
async fn late_wakeups_catch_up_on_the_audio_clock_instead_of_stretching_frames() {
    let mut reply = ReplyAudio::new(200).unwrap();
    for _ in 0..61 {
        reply.push(&[0; 320]).unwrap();
    }
    reply.finish().unwrap();
    let start = Instant::now();
    let mut count = 0_u64;
    while let Some(frame) = reply.next_frame().await {
        assert_eq!(frame.len(), 960);
        count += 1;
        // A coarse OS timer: every send is followed by an 11 ms stall.
        tokio::time::advance(Duration::from_millis(11)).await;
    }
    assert_eq!(count, 61);
    // An end-relative pacer takes 61 * 31 ms; the clock keeps real time.
    assert!(
        Instant::now() - start <= Duration::from_millis(20 * 61),
        "{:?}",
        Instant::now() - start
    );
    assert!(reply.drained());
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
