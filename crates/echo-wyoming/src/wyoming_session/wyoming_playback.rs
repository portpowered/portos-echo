use super::Reply;
use crate::{ClientOptions, Error, PlayOutcome, wyoming_queue::FrameQueue};
use echo_contracts::wyoming::AudioFormat;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

pub(super) struct Playback {
    pub(super) format: AudioFormat,
    pub(super) frames: Arc<FrameQueue<Vec<u8>>>,
    pub(super) reply: Reply<PlayOutcome>,
    pub(super) stopped: bool,
    pub(super) cut: bool,
    pump: Pump,
    stop: Option<oneshot::Sender<()>>,
    source_dropped: Arc<AtomicU64>,
    discarded: bool,
}

struct Pump(JoinHandle<()>);
impl Drop for Pump {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Playback {
    pub(super) fn new(
        format: AudioFormat,
        mut source: mpsc::Receiver<Vec<u8>>,
        reply: Reply<PlayOutcome>,
        opts: &ClientOptions,
    ) -> Self {
        let frames = Arc::new(FrameQueue::new(opts.reply_capacity));
        let queue = frames.clone();
        let limit = opts.max_frame_bytes;
        let (stop, mut stopped) = oneshot::channel();
        let source_dropped = Arc::new(AtomicU64::new(0));
        let remainder = source_dropped.clone();
        let pump = Pump(tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    biased;
                    _ = &mut stopped => {
                        source.close();
                        let mut count = 0;
                        while source.try_recv().is_ok() { count += 1; }
                        remainder.store(count, Ordering::Release);
                        return;
                    },
                    frame = source.recv() => frame,
                };
                let Some(frame) = frame else { break };
                if frame.len() > limit {
                    queue.close(Err(Error::Capacity));
                    return;
                }
                queue.push(frame);
            }
            queue.close(Ok(()));
        }));
        Self {
            format,
            frames,
            reply,
            stopped: false,
            cut: false,
            pump,
            stop: Some(stop),
            source_dropped,
            discarded: false,
        }
    }

    pub(super) async fn quiesce(&mut self) -> Result<u64, Error> {
        if self.discarded {
            return Ok(0);
        }
        self.discarded = true;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // The pump owns the source until this join. It cannot retain a frame
        // between recv and push across an await. The session alone writes audio.
        (&mut self.pump.0).await.map_err(|_| Error::Closed)?;
        if let Some(error) = self.frames.failure() {
            return Err(error);
        }
        Ok(self.source_dropped.load(Ordering::Acquire) + self.frames.discard())
    }

    #[cfg(test)]
    pub(super) fn discard(&self) {
        self.pump.0.abort();
        self.frames.discard();
    }

    pub(super) fn finish(self, result: Result<PlayOutcome, Error>) {
        let _ = self.reply.send(result);
    }
}

#[cfg(test)]
mod wyoming_tests {
    use super::*;

    fn format() -> AudioFormat {
        serde_json::from_value(serde_json::json!({"rate":24000,"width":2,"channels":1})).unwrap()
    }

    #[tokio::test]
    async fn cut_stats_recover_unpolled_source_and_repeat_is_zero() {
        let (tx, rx) = mpsc::channel(8);
        for value in 0..8 {
            tx.send(vec![value]).await.unwrap();
        }
        let (reply, _result) = oneshot::channel();
        let mut play = Playback::new(format(), rx, reply, &ClientOptions::default());
        assert_eq!(play.quiesce().await.unwrap(), 8);
        assert!(tx.is_closed());
        assert_eq!(play.frames.recv().await, Ok(None));
        assert_eq!(play.quiesce().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn cut_stats_exclude_old_evictions_and_include_remaining_source() {
        let opts = ClientOptions {
            reply_capacity: 2,
            ..ClientOptions::default()
        };
        let (tx, rx) = mpsc::channel(1);
        let (reply, _result) = oneshot::channel();
        let mut play = Playback::new(format(), rx, reply, &opts);
        for value in 0..5 {
            tx.send(vec![value]).await.unwrap();
        }
        // Reserve proves the fifth frame reached the pump and was synchronously
        // pushed. Enqueue one more without yielding, then request quiescence.
        let permit = tx.reserve().await.unwrap();
        assert_eq!(
            play.frames.stats(),
            crate::RunStats {
                received: 5,
                dropped: 3
            }
        );
        permit.send(vec![5]);
        assert_eq!(play.quiesce().await.unwrap(), 3);
        assert_eq!(play.source_dropped.load(Ordering::Acquire), 1);
        assert_eq!(play.frames.recv().await, Ok(None));
    }

    #[tokio::test]
    async fn reply_pump_keeps_newest_two_and_rejects_oversized_payload() {
        for oversized in [false, true] {
            let opts = ClientOptions {
                reply_capacity: 2,
                max_frame_bytes: 8,
                ..ClientOptions::default()
            };
            let (tx, rx) = mpsc::channel(5);
            for value in 1..=5 {
                tx.send(vec![value]).await.unwrap();
            }
            drop(tx);
            let (reply, _result) = tokio::sync::oneshot::channel();
            let mut play = Playback::new(format(), rx, reply, &opts);
            (&mut play.pump.0).await.unwrap();
            assert_eq!(
                play.frames.stats(),
                crate::RunStats {
                    received: 5,
                    dropped: 3
                }
            );
            assert_eq!(play.frames.recv().await.unwrap(), Some(vec![4]));
            assert_eq!(play.frames.recv().await.unwrap(), Some(vec![5]));
            assert_eq!(play.frames.recv().await.unwrap(), None);
            if oversized {
                let (tx, rx) = mpsc::channel(1);
                tx.send(vec![0; 9]).await.unwrap();
                let (reply, _result) = tokio::sync::oneshot::channel();
                let mut invalid = Playback::new(format(), rx, reply, &opts);
                (&mut invalid.pump.0).await.unwrap();
                assert_eq!(invalid.frames.recv().await, Err(Error::Capacity));
            }
            play.discard();
            assert_eq!(play.frames.recv().await.unwrap(), None);
        }
    }
}
