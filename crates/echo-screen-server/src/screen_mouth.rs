//! Lip-sync batches for streams that ask for them with `mouth=1`. Sending is
//! for the audio path: it never waits, and a busy or slow screen loses the
//! batch instead of delaying or queueing anything.
use crate::screen_core::Core;
use axum::{extract::Query, http::Uri};
use echo_screen_contract::mouth::{MouthBatch, MouthSample, MouthVowel};
use std::sync::Arc;
use tokio::sync::Mutex;

const HOT_SAMPLES: usize = 5;
const EMPTY_SAMPLE: MouthSample = MouthSample {
    at_ms: 0,
    open: 0,
    vowel: MouthVowel::None,
};

#[derive(Clone)]
pub(crate) struct Samples {
    sent_ms: u64,
    data: SampleStorage,
}

#[derive(Clone)]
enum SampleStorage {
    Inline {
        values: [MouthSample; HOT_SAMPLES],
        len: usize,
    },
    Extended(Arc<[MouthSample]>),
}

impl Samples {
    pub(crate) fn new(sent_ms: u64, samples: &[MouthSample]) -> Self {
        let data = if samples.len() <= HOT_SAMPLES {
            let mut values = [EMPTY_SAMPLE; HOT_SAMPLES];
            values[..samples.len()].copy_from_slice(samples);
            SampleStorage::Inline {
                values,
                len: samples.len(),
            }
        } else {
            SampleStorage::Extended(samples.into())
        };
        Self { sent_ms, data }
    }

    pub(crate) fn json(&self) -> String {
        #[derive(serde::Serialize)]
        struct Batch<'a> {
            sent_ms: u64,
            samples: &'a [MouthSample],
        }

        let samples = match &self.data {
            SampleStorage::Inline { values, len } => &values[..*len],
            SampleStorage::Extended(samples) => samples,
        };
        serde_json::to_string(&Batch {
            sent_ms: self.sent_ms,
            samples,
        })
        .unwrap_or_default()
    }
}

/// `/events?...&mouth=1` opts one stream in. Streams without it, such as an
/// older page, never receive the named `mouth` event.
pub(crate) fn wanted(uri: &Uri) -> bool {
    Query::<Vec<(String, String)>>::try_from_uri(uri)
        .is_ok_and(|Query(pairs)| pairs.iter().any(|(name, value)| name == "mouth" && value == "1"))
}

#[derive(Clone)]
pub struct MouthSender {
    core: Arc<Mutex<Core>>,
}

impl MouthSender {
    pub(crate) fn new(core: Arc<Mutex<Core>>) -> Self {
        Self { core }
    }

    /// Some open stream asked for lip sync. None while the server is busy
    /// this instant: the caller keeps what it last knew. Never waits.
    #[must_use]
    pub fn wanted(&self) -> Option<bool> {
        self.core.try_lock().ok().map(|core| core.mouth_wanted())
    }

    /// Offer the batch to every stream that asked for lip sync and has room.
    /// Returns how many took it: 0 when nobody listens, when every listener
    /// is behind, or when the server is busy this instant. Never waits.
    #[must_use]
    pub fn send(&self, batch: &MouthBatch) -> usize {
        self.send_samples(batch.sent_ms, &batch.samples)
    }

    /// Offer a borrowed batch, stored inline for the audio path's five samples.
    #[must_use]
    pub fn send_samples(&self, sent_ms: u64, samples: &[MouthSample]) -> usize {
        let Ok(core) = self.core.try_lock() else {
            return 0;
        };
        if !core.mouth_wanted() {
            return 0;
        }
        core.offer_mouth_samples(sent_ms, samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_contracts::secret::Secret;
    use echo_screen_contract::{ScreenFrame, UiState};
    use tokio::sync::watch;

    fn batch(sent_ms: u64) -> MouthBatch {
        MouthBatch {
            sent_ms,
            samples: vec![MouthSample {
                at_ms: sent_ms + 200,
                open: 80,
                vowel: MouthVowel::A,
            }],
        }
    }

    fn sender() -> MouthSender {
        MouthSender::new(Arc::new(Mutex::new(Core::new(&Secret::new(
            "synthetic-screen-pair".into(),
        )))))
    }

    #[test]
    fn only_an_exact_mouth_flag_opts_a_stream_in() {
        for (uri, expected) in [
            ("/events?token=t&mouth=1", true),
            ("/events?mouth=1&token=t", true),
            ("/events?token=t", false),
            ("/events?token=t&mouth=0", false),
            ("/events?token=t&mouth=", false),
            ("/events?token=t&mouths=1", false),
            ("/events", false),
        ] {
            assert_eq!(wanted(&uri.parse().unwrap()), expected, "{uri}");
        }
    }

    #[tokio::test]
    async fn batches_reach_only_streams_that_asked_and_leave_frames_alone() {
        let sender = sender();
        assert_eq!(sender.send(&batch(1)), 0);
        let mut core = sender.core.lock().await;
        let mut plain = core.attach(watch::channel(false).0).unwrap();
        assert!(!core.mouth_wanted());
        let mut lips = core.attach_with(watch::channel(false).0, true).unwrap();
        assert!(core.publish(UiState::Speaking));
        drop(core);
        assert_eq!(sender.send(&batch(1000)), 1);
        let mut core = sender.core.lock().await;
        assert!(core.publish(UiState::Connected));
        // The plain stream saw two frames and nothing else.
        for seq in 1..=2 {
            let frame: ScreenFrame = serde_json::from_str(&plain.frames.recv().await.unwrap().data()).unwrap();
            assert_eq!(frame.seq, seq);
        }
        assert!(plain.frames.try_recv().is_err());
        assert_eq!(lips.frames.recv().await.unwrap().name(), None);
        let event = lips.frames.recv().await.unwrap();
        assert_eq!(event.name(), Some("mouth"));
        assert_eq!(
            event.data(),
            r#"{"sent_ms":1000,"samples":[{"at_ms":1200,"open":80,"vowel":"a"}]}"#
        );
        // The batch took no sequence number and is not the reconnect snapshot.
        let frame: ScreenFrame = serde_json::from_str(&lips.frames.recv().await.unwrap().data()).unwrap();
        assert_eq!(frame.seq, 2);
        let mut late = core.attach_with(watch::channel(false).0, true).unwrap();
        assert_eq!(late.frames.recv().await.unwrap().name(), None);
        assert!(late.frames.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_stalled_screen_loses_batches_and_keeps_its_stream_and_frames() {
        let sender = sender();
        let mut stalled = sender
            .core
            .lock()
            .await
            .attach_with(watch::channel(false).0, true)
            .unwrap();
        // Nobody reads: eight batches fit before the reserve for frames.
        let taken: Vec<usize> = (0..40).map(|index| sender.send(&batch(index))).collect();
        assert_eq!(taken.iter().sum::<usize>(), 8);
        assert_eq!(taken[..8], [1; 8]);
        assert!(!*stalled.cancelled.borrow());
        let mut core = sender.core.lock().await;
        assert!(core.mouth_wanted());
        // Every reserved slot still takes a screen frame.
        for _ in 0..24 {
            assert!(core.publish(UiState::Speaking));
        }
        assert!(!*stalled.cancelled.borrow());
        assert_eq!(stalled.frames.len(), 32);
        drop(core);
        // Once it reads again, the newest batches flow.
        for sent in 0..8 {
            let event = stalled.frames.recv().await.unwrap();
            assert!(event.data().starts_with(&format!(r#"{{"sent_ms":{sent},"#)));
        }
        assert_eq!(sender.send(&batch(99)), 0);
        for _ in 0..24 {
            assert_eq!(stalled.frames.recv().await.unwrap().name(), None);
        }
        assert_eq!(sender.send(&batch(100)), 1);
    }

    #[tokio::test]
    async fn a_busy_draining_or_revoked_server_takes_nothing_without_waiting() {
        let sender = sender();
        let mut client = sender
            .core
            .lock()
            .await
            .attach_with(watch::channel(false).0, true)
            .unwrap();
        assert_eq!(sender.wanted(), Some(true));
        let held = sender.core.lock().await;
        assert_eq!((sender.wanted(), sender.send(&batch(1))), (None, 0));
        drop(held);
        assert_eq!(sender.send(&batch(2)), 1);
        sender.core.lock().await.drain();
        assert_eq!(sender.send(&batch(3)), 0);
        sender.core.lock().await.revoke();
        assert_eq!(sender.send(&batch(4)), 0);
        assert!(client.frames.recv().await.unwrap().data().contains(r#""sent_ms":2"#));
        assert!(client.frames.try_recv().is_err());
    }
}
