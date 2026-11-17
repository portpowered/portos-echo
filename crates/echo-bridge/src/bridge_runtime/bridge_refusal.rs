//! Bounded service turns for the single call owner, even with inexhaustible input.
use std::{future::Future, task::Poll};

pub(super) enum Work<M, C, R> {
    Microphone(M),
    Control(C),
    Reply(R),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{pending, ready};

    #[tokio::test(start_paused = true)]
    async fn ready_input_cannot_hide_pre_pcm_failure() {
        let mut arbiter = Arbiter::default();
        let mut microphone = 0;
        let mut failure = None;
        for _ in 0..3 {
            match arbiter
                .next(ready(960), ready(Err::<(), _>("unavailable")), pending::<()>())
                .await
            {
                Work::Microphone(samples) => microphone += samples,
                Work::Control(result) => {
                    failure = Some(result);
                    break;
                }
                Work::Reply(()) => unreachable!(),
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
            let selected = match arbiter.next(ready(index), ready(index), ready(index)).await {
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
                arbiter.next(ready(index), &mut receive, pending::<()>()).await,
                Work::Microphone(value) if value == index
            ));
        }
        {
            let next = arbiter.next(pending::<()>(), &mut receive, pending::<()>());
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
            arbiter.next(ready(4), pending::<()>(), pending::<()>()).await,
            Work::Microphone(4)
        ));
    }
}

#[derive(Default)]
pub(super) struct Arbiter(usize);

impl Arbiter {
    pub async fn next<M: Future, C: Future, R: Future>(
        &mut self,
        microphone: M,
        control: C,
        reply: R,
    ) -> Work<M::Output, C::Output, R::Output> {
        let mut microphone = std::pin::pin!(microphone);
        let mut control = std::pin::pin!(control);
        let mut reply = std::pin::pin!(reply);
        std::future::poll_fn(|cx| {
            for offset in 0..3 {
                let index = (self.0 + offset) % 3;
                let polled = match index {
                    0 => microphone.as_mut().poll(cx).map(Work::Microphone),
                    1 => control.as_mut().poll(cx).map(Work::Control),
                    _ => reply.as_mut().poll(cx).map(Work::Reply),
                };
                if polled.is_ready() {
                    self.0 = (index + 1) % 3;
                    return polled;
                }
            }
            Poll::Pending
        })
        .await
    }
}
