//! Bounded service turns for the single call owner, even with inexhaustible input.
use std::{future::Future, task::Poll};

pub(super) enum Work<M, C, R, D> {
    Microphone(M),
    Control(C),
    Reply(R),
    Device(D),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{pending, ready};

    #[tokio::test(start_paused = true)]
    async fn ready_device_notifications_share_turns_with_microphone_live_and_reply() {
        let mut arbiter = Arbiter::default();
        let mut counts = [0; 4];
        for index in 0..40 {
            let selected = match arbiter
                .next(ready(index), ready(index), ready(index), ready(index))
                .await
            {
                Work::Microphone(value) => {
                    assert_eq!(value, index);
                    0
                }
                Work::Control(value) => {
                    assert_eq!(value, index);
                    1
                }
                Work::Reply(value) => {
                    assert_eq!(value, index);
                    2
                }
                Work::Device(value) => {
                    assert_eq!(value, index);
                    3
                }
            };
            assert_eq!(selected, index % 4);
            counts[selected] += 1;
        }
        assert_eq!(counts, [10; 4]);
    }

    #[tokio::test(start_paused = true)]
    async fn ready_input_cannot_hide_pre_pcm_failure() {
        let mut arbiter = Arbiter::default();
        let mut microphone = 0;
        let mut failure = None;
        for _ in 0..3 {
            match arbiter
                .next(
                    ready(960),
                    ready(Err::<(), _>("unavailable")),
                    pending::<()>(),
                    pending::<()>(),
                )
                .await
            {
                Work::Microphone(samples) => microphone += samples,
                Work::Control(result) => {
                    failure = Some(result);
                    break;
                }
                Work::Reply(()) | Work::Device(()) => unreachable!(),
            }
        }
        assert_eq!(
            failure,
            Some(Err("unavailable")),
            "ready failure starved while microphone remained offered"
        );
        assert_eq!(microphone, 960);
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_controls_preserve_microphone_and_due_reply_turns() {
        let mut arbiter = Arbiter::default();
        let mut counts = [0; 3];
        for index in 0..30 {
            let selected = match arbiter
                .next(ready(index), ready(index), ready(index), pending::<()>())
                .await
            {
                Work::Microphone(value) => {
                    assert_eq!(value, index);
                    0
                }
                Work::Control(value) => {
                    assert_eq!(value, index);
                    1
                }
                Work::Reply(value) => {
                    assert_eq!(value, index);
                    2
                }
                Work::Device(()) => unreachable!(),
            };
            assert_eq!(selected, index % 3);
            counts[selected] += 1;
        }
        assert_eq!(counts, [10, 10, 10]);
    }

    #[tokio::test(start_paused = true)]
    async fn pending_control_registers_wakeup_without_stalling_input() {
        let mut arbiter = Arbiter::default();
        let (send, mut receive) = tokio::sync::oneshot::channel();
        for index in 0..4 {
            assert!(matches!(
                arbiter.next(ready(index), &mut receive, pending::<()>(), pending::<()>()).await,
                Work::Microphone(value) if value == index
            ));
        }
        {
            let next = arbiter.next(pending::<()>(), &mut receive, pending::<()>(), pending::<()>());
            tokio::pin!(next);
            std::future::poll_fn(|cx| {
                assert!(next.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            send.send("delegation").unwrap();
            assert!(matches!(next.await, Work::Control(Ok("delegation"))));
        }
        assert!(matches!(
            arbiter
                .next(ready(4), pending::<()>(), pending::<()>(), pending::<()>())
                .await,
            Work::Microphone(4)
        ));
    }

    #[tokio::test]
    async fn ready_input_allows_background_control_delivery() {
        let mut arbiter = Arbiter::default();
        let (send, mut receive) = tokio::sync::oneshot::channel();
        let delivery = tokio::spawn(async move { send.send("delegation").unwrap() });
        let mut frames = 0;
        let mut delivered = false;
        for _ in 0..8 {
            match arbiter
                .next(ready(960), &mut receive, pending::<()>(), pending::<()>())
                .await
            {
                Work::Microphone(samples) => frames += samples,
                Work::Control(value) => {
                    assert_eq!(value.unwrap(), "delegation");
                    delivered = true;
                    break;
                }
                Work::Reply(()) | Work::Device(()) => unreachable!(),
            }
        }
        assert!(delivered, "ready capture monopolized the control task's executor");
        assert!(frames > 0);
        delivery.await.unwrap();
        assert!(matches!(
            arbiter
                .next(ready(960), pending::<()>(), pending::<()>(), pending::<()>())
                .await,
            Work::Microphone(960)
        ));
    }
}

#[derive(Default)]
pub(super) struct Arbiter(usize);

impl Arbiter {
    pub async fn next<M: Future, C: Future, R: Future, D: Future>(
        &mut self,
        microphone: M,
        control: C,
        reply: R,
        device: D,
    ) -> Work<M::Output, C::Output, R::Output, D::Output> {
        // Device notifications compete with their own microphone payloads.
        // Keep them inside this rotation: an outer biased select can otherwise
        // cancel every yielding microphone turn while the bounded queue fills.
        // Ready audio must also share the executor with delegation and socket
        // tasks, rather than spend an entire cooperative budget converting PCM.
        tokio::task::yield_now().await;
        let mut microphone = std::pin::pin!(microphone);
        let mut control = std::pin::pin!(control);
        let mut reply = std::pin::pin!(reply);
        let mut device = std::pin::pin!(device);
        std::future::poll_fn(|cx| {
            for offset in 0..4 {
                let index = (self.0 + offset) % 4;
                let polled = match index {
                    0 => microphone.as_mut().poll(cx).map(Work::Microphone),
                    1 => control.as_mut().poll(cx).map(Work::Control),
                    2 => reply.as_mut().poll(cx).map(Work::Reply),
                    _ => device.as_mut().poll(cx).map(Work::Device),
                };
                if polled.is_ready() {
                    self.0 = (index + 1) % 4;
                    return polled;
                }
            }
            Poll::Pending
        })
        .await
    }
}
