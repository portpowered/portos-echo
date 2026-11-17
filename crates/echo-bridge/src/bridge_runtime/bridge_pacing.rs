use echo_audio::{FrameQueue, Pacer};
use std::time::Duration;
use tokio::time::Instant;

/// Initial playback credit followed by the existing no-catch-up 20 ms pacer.
pub(crate) struct LeadPacer {
    remaining: u16,
    started: Option<Instant>,
    pacer: Pacer,
}

impl LeadPacer {
    pub(crate) fn new(lead_ms: u16) -> Self {
        Self {
            remaining: lead_ms / 20,
            started: None,
            pacer: Pacer::new(),
        }
    }

    pub(crate) async fn next_frame<T>(&mut self, queue: &mut FrameQueue<T>) -> Option<T> {
        if queue.is_empty() {
            return None;
        }
        let started = *self.started.get_or_insert_with(|| {
            self.pacer = Pacer::new();
            Instant::now()
        });
        // Underflow never renews credit, and a late producer never catches up.
        if Instant::now() >= started + Duration::from_millis(20) {
            self.remaining = 0;
        }
        if self.remaining > 0 {
            self.remaining -= 1;
            queue.pop()
        } else {
            self.pacer.next_frame(queue).await
        }
    }
}

#[cfg(test)]
mod bridge_pacing_tests {
    use super::*;
    use std::{future::Future, num::NonZeroUsize, task::Poll};

    fn queue() -> FrameQueue<u16> {
        let mut queue = FrameQueue::new(NonZeroUsize::new(30).unwrap());
        for frame in 0..30 {
            queue.push(frame);
        }
        queue
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_ten_initial_frames_then_twenty_ms_without_catchup() {
        let mut queue = queue();
        let mut pacer = LeadPacer::new(200);
        let start = Instant::now();
        for frame in 0..10 {
            assert_eq!(pacer.next_frame(&mut queue).await, Some(frame));
        }
        assert_eq!(Instant::now(), start);
        assert_eq!(pacer.next_frame(&mut queue).await, Some(10));
        assert_eq!(Instant::now() - start, Duration::from_millis(20));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(pacer.next_frame(&mut queue).await, Some(11));
        let late = Instant::now();
        assert_eq!(pacer.next_frame(&mut queue).await, Some(12));
        assert_eq!(Instant::now() - late, Duration::from_millis(20));
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_underflow_and_cancel_never_add_credit_or_consume_frames() {
        let mut queue = queue();
        let mut pacer = LeadPacer::new(20);
        assert_eq!(pacer.next_frame(&mut queue).await, Some(0));
        let mut next = Box::pin(pacer.next_frame(&mut queue));
        std::future::poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(next);
        assert_eq!(queue.len(), 29);
        queue.clear();
        assert_eq!(pacer.next_frame(&mut queue).await, None);
        tokio::time::advance(Duration::from_secs(1)).await;
        queue.push(40);
        queue.push(41);
        let now = Instant::now();
        assert_eq!(pacer.next_frame(&mut queue).await, Some(40));
        assert_eq!(pacer.next_frame(&mut queue).await, Some(41));
        assert_eq!(Instant::now() - now, Duration::from_millis(20));
    }
}
