use super::*;
use std::{future::pending, sync::Arc, time::Duration};

fn id() -> TaskId {
    TaskId::new("unit-task").unwrap()
}

#[tokio::test]
async fn terminal_does_not_wait_for_progress_consumer_and_first_wins() {
    let events = Events::new(&id());
    events.finish(&id(), Ok("done".into()));
    events.finish(&id(), Err("task timed out"));
    events.cancel(&id()).await;
    assert_eq!(
        events.next().await,
        Some(TaskEvent::Progress {
            task_id: id(),
            message: "task started".into()
        })
    );
    assert_eq!(
        events.next().await,
        Some(TaskEvent::Completed {
            task_id: id(),
            summary: "done".into()
        })
    );
    assert_eq!(events.next().await, None);
    assert_eq!(lock(&events.state).queue.len(), 0);
    assert!(lock(&events.state).worker.is_none());
}

#[tokio::test(start_paused = true)]
async fn deadline_precedes_ready_work_including_zero_and_cancelled_poll() {
    for seconds in [0, 5] {
        let events = Events::new(&id());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        tokio::time::advance(Duration::from_secs(seconds)).await;
        let polled = std::sync::atomic::AtomicBool::new(false);
        let run = async {
            polled.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("too late".into())
        };
        assert_eq!(events.drive(run, deadline).await, Err("task timed out"));
        assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));
        events.cancel(&id()).await;
        assert_eq!(events.drive(pending(), deadline).await, Err("task cancelled"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_and_two_cancels_arbitrate_one_terminal() {
    let events = Arc::new(Events::new(&id()));
    let worker_events = events.clone();
    let worker = tokio::spawn(async move {
        worker_events.finish(&id(), Ok("done".into()));
    });
    events.set_worker(worker);
    let task_id = id();
    tokio::join!(events.cancel(&task_id), events.cancel(&task_id));
    assert_eq!(
        events.next().await,
        Some(TaskEvent::Progress {
            task_id: id(),
            message: "task started".into()
        })
    );
    let result = events.next().await;
    assert!(
        result
            == Some(TaskEvent::Completed {
                task_id: id(),
                summary: "done".into()
            })
            || result
                == Some(TaskEvent::Failed {
                    task_id: id(),
                    retryable: false,
                    message: "task cancelled".into()
                })
    );
    assert_eq!(events.next().await, None);
}

#[tokio::test]
async fn waiter_wakes_and_worker_attached_after_stop_is_aborted() {
    let events = Arc::new(Events::new(&id()));
    events.next().await;
    let waiting = events.clone();
    let waiter = tokio::spawn(async move { waiting.next().await });
    tokio::task::yield_now().await;
    events.finish(&id(), Err("task cancelled"));
    assert_eq!(
        waiter.await.unwrap(),
        Some(TaskEvent::Failed {
            task_id: id(),
            retryable: false,
            message: "task cancelled".into()
        })
    );
    let worker = tokio::spawn(pending());
    let abort = worker.abort_handle();
    events.set_worker(worker);
    tokio::task::yield_now().await;
    assert!(abort.is_finished());
}
