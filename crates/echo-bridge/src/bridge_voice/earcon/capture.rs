//! Preserve setup PCM while advancing the adapter's ordered mute boundary.
use super::current;
use crate::Error;
use echo_link::{MicFrame, MicrophoneRun};
use std::{collections::VecDeque, future::Future};

// Ephemeral per-wake storage, never an unbounded extension of device capture.
const MAX_BYTES: usize = 1_048_576;
const MAX_FRAMES: usize = 1024;

#[derive(Default)]
pub(super) struct Capture {
    frames: VecDeque<MicFrame>,
    bytes: usize,
    ended: bool,
}

impl Capture {
    fn push(&mut self, frame: Option<MicFrame>) -> Result<(), Error> {
        if let Some(frame) = frame {
            let bytes = self.bytes.checked_add(frame.payload.len()).ok_or(Error::Wyoming)?;
            if bytes > MAX_BYTES || self.frames.len() == MAX_FRAMES {
                return Err(echo_link::Error::Capacity.into());
            }
            self.bytes = bytes;
            self.frames.push_back(frame);
        } else {
            self.ended = true;
        }
        Ok(())
    }
}

pub(in crate::bridge_voice) async fn during<T>(
    microphone: &mut impl MicrophoneRun,
    work: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    let mut work = Box::pin(work);
    let Some(context) = current() else {
        return work.await;
    };
    loop {
        let ended = context.state().capture.ended;
        tokio::select! {
            biased;
            result = &mut work => return result,
            frame = microphone.recv(), if !ended => context.state().capture.push(frame?)?,
        }
    }
}

pub(in crate::bridge_voice) async fn recv(
    microphone: &mut impl MicrophoneRun,
) -> Result<Option<MicFrame>, echo_link::Error> {
    if let Some(context) = current() {
        let mut state = context.state();
        if let Some(frame) = state.capture.frames.pop_front() {
            state.capture.bytes -= frame.payload.len();
            return Ok(Some(frame));
        }
        if state.capture.ended {
            return Ok(None);
        }
    }
    microphone.recv().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_link::{
        EchoLink, LinkConnector, LinkEvent, LinkEvents, PcmFormat,
        fake::{FakeConnector, Observer},
    };

    fn frame(value: i16) -> MicFrame {
        MicFrame {
            format: PcmFormat {
                rate: 16000,
                width: 2,
                channels: 1,
            },
            payload: value.to_le_bytes().repeat(320),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn wake_earcon_setup_replays_exact_pcm_and_eof_with_unchanged_stats() {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        observer.wake_with_mute("buffered", vec![frame(123), frame(-456)], 2);
        let LinkEvent::Wake(mut run) = events.recv().await.unwrap().unwrap() else {
            unreachable!()
        };
        let context = super::super::Context::default();
        super::super::CURRENT
            .scope(context.clone(), async {
                let work = async {
                    while !context.state().capture.ended {
                        tokio::task::yield_now().await;
                    }
                    Ok(())
                };
                during(&mut run, work).await.unwrap();
                assert_eq!(context.state().capture.bytes, 1280);
                assert_eq!(recv(&mut run).await.unwrap(), Some(frame(123)));
                assert_eq!(recv(&mut run).await.unwrap(), Some(frame(-456)));
                assert_eq!(recv(&mut run).await.unwrap(), None);
                assert_eq!(context.state().capture.bytes, 0);
                assert_eq!(
                    run.stats(),
                    echo_link::RunStats {
                        received: 2,
                        dropped: 0
                    }
                );
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn wake_earcon_setup_drains_ordered_premute_frames_before_mute_cancels_setup() {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        observer.wake_with_mute("muted", vec![frame(123), frame(456)], 1);
        let LinkEvent::Wake(mut run) = events.recv().await.unwrap().unwrap() else {
            unreachable!()
        };
        let context = super::super::Context::default();
        super::super::CURRENT
            .scope(context.clone(), async {
                let mut muted = false;
                let result = super::super::negotiating(
                    &mut events,
                    during(&mut run, std::future::pending::<Result<(), Error>>()),
                    &mut |notice| {
                        assert!(matches!(
                            notice,
                            crate::bridge_voice::SessionNotice::Run(crate::bridge_voice::Notice::Device(
                                LinkEvent::Mute(true)
                            ))
                        ));
                        muted = true;
                        Ok(())
                    },
                )
                .await;
                assert_eq!(result, Err(Error::Control));
                assert!(muted);
                assert!(context.state().stop);
                assert_eq!(recv(&mut run).await.unwrap(), Some(frame(123)));
                assert_eq!(recv(&mut run).await.unwrap(), None);
                assert_eq!(
                    run.stats(),
                    echo_link::RunStats {
                        received: 2,
                        dropped: 1
                    }
                );
            })
            .await;
    }

    #[test]
    fn wake_earcon_setup_capacity_fails_without_evicting_retained_pcm() {
        let mut capture = Capture::default();
        let mut large = frame(123);
        large.payload.resize(MAX_BYTES, 1);
        capture.push(Some(large)).unwrap();
        assert_eq!(capture.push(Some(frame(456))), Err(echo_link::Error::Capacity.into()));
        assert_eq!((capture.bytes, capture.frames.len()), (MAX_BYTES, 1));
        let mut capture = Capture::default();
        for _ in 0..MAX_FRAMES {
            capture.push(Some(frame(123))).unwrap();
        }
        assert_eq!(capture.push(Some(frame(456))), Err(echo_link::Error::Capacity.into()));
        assert_eq!(capture.frames.len(), MAX_FRAMES);
    }
}
