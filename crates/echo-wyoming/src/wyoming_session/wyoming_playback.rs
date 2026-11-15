use super::Reply;
use crate::{ClientOptions, Error, PlayOutcome, wyoming_queue::FrameQueue};
use echo_contracts::wyoming::AudioFormat;
use std::sync::Arc;
use tokio::{sync::mpsc, task::JoinHandle};

pub(super) struct Playback {
    pub(super) format: AudioFormat,
    pub(super) frames: Arc<FrameQueue<Vec<u8>>>,
    pub(super) reply: Reply<PlayOutcome>,
    pub(super) stopped: bool,
    pub(super) cut: bool,
    pump: Pump,
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
        let pump = Pump(tokio::spawn(async move {
            while let Some(frame) = source.recv().await {
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
        }
    }

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
