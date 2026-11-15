use super::*;
use std::{
    future::Future,
    io::Write,
    num::NonZeroUsize,
    pin::pin,
    task::{Context, Waker},
};

fn pending(future: impl Future) {
    assert!(
        pin!(future)
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
}

#[tokio::test(start_paused = true)]
async fn fifty_deadlines_without_early_emission() {
    let start = Instant::now();
    let mut pacer = Pacer::default();
    let mut queue = FrameQueue::new(NonZeroUsize::new(50).unwrap());
    for frame in 0..50 {
        queue.push(frame);
    }
    pending(pacer.next_frame(&mut queue));
    assert_eq!(queue.len(), 50);
    tokio::time::advance(Duration::from_millis(19)).await;
    pending(pacer.next_frame(&mut queue));
    assert_eq!(queue.len(), 50);
    for frame in 0..50 {
        assert_eq!(pacer.next_frame(&mut queue).await, Some(frame));
        assert_eq!(Instant::now() - start, Duration::from_millis((frame + 1) * 20));
        pending(pacer.next_frame(&mut queue));
    }
    assert_eq!(queue.len(), 0);
    assert_eq!(Instant::now() - start, Duration::from_secs(1));
    writeln!(
        std::io::stdout(),
        "paced_frames=50 simulated_ms=1000 first_deadline_ms=20"
    )
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn late_poll_skips_burst_and_underflow_waits() {
    let mut pacer = Pacer::new();
    let mut queue = FrameQueue::new(NonZeroUsize::new(3).unwrap());
    queue.push(0);
    queue.push(1);
    tokio::time::advance(Duration::from_millis(105)).await;
    assert_eq!(pacer.next_frame(&mut queue).await, Some(0));
    pending(pacer.next_frame(&mut queue));
    assert_eq!(queue.len(), 1);
    let late = Instant::now();
    assert_eq!(pacer.next_frame(&mut queue).await, Some(1));
    assert_eq!(Instant::now() - late, PERIOD);
    assert_eq!(pacer.next_frame(&mut queue).await, None);
    assert_eq!(Instant::now() - late, PERIOD * 2);
    queue.push(2);
    pending(pacer.next_frame(&mut queue));
    assert_eq!(queue.pop(), Some(2));
}

#[tokio::test(start_paused = true)]
async fn cancellation_retains_deadline_and_payload() {
    let mut queue = FrameQueue::new(NonZeroUsize::new(1).unwrap());
    queue.push([7; 960]);
    let mut pacer = Pacer::new();
    let start = Instant::now();
    pending(pacer.next_frame(&mut queue));
    tokio::time::advance(Duration::from_millis(10)).await;
    // A separate scope ensures the cancelled future releases its queue borrow.
    assert_eq!(pacer.next_frame(&mut queue).await, Some([7; 960]));
    assert_eq!(Instant::now() - start, PERIOD);
}
