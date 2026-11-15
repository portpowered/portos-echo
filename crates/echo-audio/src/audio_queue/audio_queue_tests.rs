use super::*;
use proptest::prelude::*;
use std::io::Write;

#[test]
fn last_n_clear_and_capacity_one() {
    assert_eq!(NonZeroUsize::new(0), None);
    let mut queue = FrameQueue::new(NonZeroUsize::new(3).unwrap());
    assert_eq!(queue.capacity(), 3);
    assert!(queue.is_empty());
    for n in 0..5 {
        assert_eq!(queue.push(n), if n >= 3 { Some(n - 3) } else { None });
    }
    assert_eq!(queue.len(), 3);
    assert_eq!(queue.drop_count(), 2);
    assert_eq!(
        [queue.pop(), queue.pop(), queue.pop(), queue.pop()],
        [Some(2), Some(3), Some(4), None]
    );
    writeln!(std::io::stdout(), "queue_capacity=3 pushed=5 retained=[2,3,4] drops=2").unwrap();
    queue.push(99);
    queue.clear();
    assert_eq!(queue.pop(), None);
    assert_eq!(queue.drop_count(), 0);
    assert_eq!(queue.capacity(), 3);
}

#[test]
fn capacity_one_and_saturated_count() {
    let mut one = FrameQueue::new(NonZeroUsize::new(1).unwrap());
    assert_eq!(one.push(1), None);
    assert_eq!(one.push(2), Some(1));
    assert_eq!(one.pop(), Some(2));
    one.drops = u64::MAX;
    one.push(3);
    assert_eq!(one.push(4), Some(3));
    assert_eq!(one.drop_count(), u64::MAX);
}

#[test]
fn operation_sequences_match_fifo_model() {
    let strategy = (
        1usize..=64,
        prop::collection::vec(prop::option::of(any::<u16>()), 0..=256),
    );
    proptest::test_runner::TestRunner::default()
        .run(&strategy, |(capacity, operations)| {
            let mut queue = FrameQueue::new(NonZeroUsize::new(capacity).unwrap());
            let mut model = VecDeque::new();
            let mut drops = 0;
            for operation in operations {
                if let Some(value) = operation {
                    let evicted = if model.len() == capacity {
                        drops += 1;
                        model.pop_front()
                    } else {
                        None
                    };
                    model.push_back(value);
                    assert_eq!(queue.push(value), evicted);
                } else {
                    assert_eq!(queue.pop(), model.pop_front());
                }
                assert_eq!(queue.len(), model.len());
                assert!(queue.len() <= capacity);
                assert_eq!(queue.drop_count(), drops);
            }
            while let Some(value) = model.pop_front() {
                assert_eq!(queue.pop(), Some(value));
            }
            assert_eq!(queue.pop(), None);
            Ok(())
        })
        .unwrap();
}
#[test]
fn loom_synchronized_producers_consumer_preserve_actual_queue_order() {
    use loom::{
        sync::{Arc, Mutex},
        thread,
    };
    loom::model(|| {
        // Expected order is recorded at the same lock boundary as insertion.
        let state = Arc::new(Mutex::new((
            FrameQueue::new(NonZeroUsize::new(1).unwrap()),
            VecDeque::new(),
            Vec::new(),
            0u64,
        )));
        let producers: Vec<_> = [10, 20]
            .into_iter()
            .map(|value| {
                let state = state.clone();
                thread::spawn(move || {
                    let mut state = state.lock().unwrap();
                    let expected = if state.1.len() == 1 {
                        state.3 += 1;
                        state.1.pop_front()
                    } else {
                        None
                    };
                    assert_eq!(state.0.push(value), expected);
                    state.1.push_back(value);
                })
            })
            .collect();
        let consumer_state = state.clone();
        let consumer = thread::spawn(move || {
            let mut state = consumer_state.lock().unwrap();
            let expected = state.1.pop_front();
            let actual = state.0.pop();
            assert_eq!(actual, expected);
            if let Some(value) = actual {
                state.2.push(value);
            }
        });
        for producer in producers {
            producer.join().unwrap();
        }
        consumer.join().unwrap();
        let mut state = state.lock().unwrap();
        let expected = state.1.pop_front();
        let actual = state.0.pop();
        assert_eq!(actual, expected);
        if let Some(value) = actual {
            state.2.push(value);
        }
        assert_eq!(state.0.pop(), None);
        assert_eq!(state.0.drop_count(), state.3);
        assert_eq!(state.2.len() as u64 + state.3, 2);
        if state.2.len() == 2 {
            assert_ne!(state.2[0], state.2[1]);
        }
    });
}
