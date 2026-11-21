use super::{LinkEvent, Observer, Poll, SpeakerState, lock, poll_fn};

impl Observer {
    /// Set the microphone state sampled by the next connection.
    pub fn muted_at_connect(&self, muted: bool) {
        lock(&self.0).initial_mute = muted.then_some(());
    }

    /// Script an idle microphone mute transition in event order.
    pub fn mute(&self, muted: bool) {
        let mut state = lock(&self.0);
        state.initial_mute = muted.then_some(());
        state.events.push_back(LinkEvent::Mute(muted));
        if let Some(waker) = state.event_waker.take() {
            waker.wake();
        }
    }

    /// Script the authoritative speaker view without acknowledging a command.
    pub fn report_speaker(&self, speaker: SpeakerState) {
        let waker = {
            let mut state = lock(&self.0);
            state.speaker = speaker;
            state.speaker_waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub(super) async fn speaker_changed(&self, previous: SpeakerState) -> SpeakerState {
        poll_fn(|cx| {
            let mut state = lock(&self.0);
            if state.speaker == previous {
                state.speaker_waker = Some(cx.waker().clone());
                Poll::Pending
            } else {
                Poll::Ready(state.speaker)
            }
        })
        .await
    }
}
