//! A grant prompt overlays activity, then restores the latest run state.
use crate::Error;
use echo_screen_contract::UiState;
use std::sync::{Arc, Mutex};
use tokio::{sync::mpsc, time::Instant};

pub(crate) const RELINK: &str = "Run echo-bridge link with your config to relink Port OS MCP.";

#[derive(Clone)]
pub(crate) struct Presentation {
    clock: Option<super::bridge_clock::receipt::Context>,
    states: Option<mpsc::Sender<UiState>>,
    current: Arc<Mutex<Projection>>,
    emotion_changed: Arc<tokio::sync::Notify>,
}

struct Projection {
    activity: UiState,
    overlay: Option<UiState>,
    openai: Option<UiState>,
    muted: bool,
    emotion: Option<(UiState, Instant)>,
}

impl Projection {
    fn visible(&self) -> UiState {
        if let Some(recovery) = &self.openai {
            let mut state = recovery.clone();
            match &mut state {
                UiState::OpenAiRelink { muted, .. } | UiState::OpenAiSignin { muted, .. } => *muted = self.muted,
                _ => (),
            }
            state
        } else if self.muted {
            UiState::Muted
        } else if let Some(overlay) = &self.overlay {
            overlay.clone()
        } else if let Some((emotion, _)) = &self.emotion {
            emotion.clone()
        } else {
            self.activity.clone()
        }
    }
}

impl Presentation {
    pub fn new(states: Option<mpsc::Sender<UiState>>) -> Self {
        Self {
            clock: super::bridge_clock::receipt::Context::current(),
            states,
            emotion_changed: Arc::default(),
            current: Arc::new(Mutex::new(Projection {
                activity: UiState::Ready {
                    message: "Ready".into(),
                },
                overlay: None,
                openai: None,
                muted: false,
                emotion: None,
            })),
        }
    }

    pub fn publish(&self, state: UiState) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        current.activity = state;
        current.emotion = None;
        self.send(current.visible())
    }

    pub fn thinking(&self) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        if current.muted || current.activity == UiState::Thinking {
            return Ok(());
        }
        current.activity = UiState::Thinking;
        if current.openai.is_none() && current.overlay.is_none() && current.emotion.is_none() {
            self.send(UiState::Thinking)?;
        }
        Ok(())
    }

    /// Drop closed-run activity before a recovery or kill overlay can clear.
    /// The view owns the next visible publication.
    pub fn clear_thinking(&self) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        if current.activity == UiState::Thinking {
            current.activity = UiState::Ready {
                message: "Echo Live ready".into(),
            };
        }
        Ok(())
    }

    pub fn overlay(&self, state: Option<UiState>) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        current.overlay = state;
        current.emotion = None;
        self.send(current.visible())
    }

    pub(crate) fn openai(&self, state: Option<UiState>) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        if current.openai == state {
            return Ok(());
        }
        current.openai = state;
        current.emotion = None;
        self.send(current.visible())
    }

    pub(crate) fn clear_approval(&self, id: &str) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        if matches!(&current.overlay, Some(UiState::Approval { id: active, .. }) if active == id) {
            current.overlay = None;
            self.send(current.visible())?;
        }
        Ok(())
    }

    pub(crate) fn approval(&self, state: UiState) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        if current.muted || current.openai.is_some() || current.overlay.is_some() {
            return Err(Error::Screen);
        }
        current.overlay = Some(state);
        current.emotion = None;
        self.send(current.visible())
    }

    pub fn muted(&self, muted: bool) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        current.muted = muted;
        current.emotion = None;
        if !muted {
            // Muting ends the run; restore idle before exposing activity again.
            current.activity = UiState::Ready {
                message: "Echo Live ready".into(),
            };
        }
        self.send(current.visible())
    }

    pub fn emotion(&self, state: UiState, deadline: Instant) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        if current.muted || current.overlay.is_some() {
            return Ok(());
        }
        current.emotion = Some((state, deadline));
        self.emotion_changed.notify_one();
        self.send(current.visible())
    }

    /// The screen owner polls this beside actions; no independent task owns it.
    pub async fn advance_emotion(&self) -> Result<(), Error> {
        loop {
            let changed = self.emotion_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            tokio::select! {
                () = &mut changed => (),
                () = self.emotion_deadline() => return self.expire_emotion(),
            }
        }
    }

    pub fn clear_emotion(&self) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        if current.emotion.take().is_some() {
            self.send(current.visible())?;
        }
        Ok(())
    }

    pub fn expire_emotion(&self) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        if current
            .emotion
            .as_ref()
            .is_some_and(|(_, deadline)| *deadline <= Instant::now())
        {
            current.emotion = None;
            self.send(current.visible())?;
        }
        Ok(())
    }

    pub async fn emotion_deadline(&self) {
        let deadline = self
            .current
            .lock()
            .ok()
            .and_then(|current| current.emotion.as_ref().map(|(_, deadline)| *deadline));
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    }

    fn send(&self, state: UiState) -> Result<(), Error> {
        if let Some(states) = &self.states {
            let projected = self
                .clock
                .as_ref()
                .and_then(|_| (!matches!(state, UiState::OpenAiSignin { .. })).then(|| state.clone()));
            states.try_send(state).map_err(|_| Error::Screen)?;
            if let (Some(clock), Some(projected)) = (&self.clock, projected) {
                clock.projected(&projected)?;
            }
        }
        Ok(())
    }

    pub fn rejected(&self, message: &str) -> Result<(), Error> {
        self.overlay(Some(UiState::Error {
            message: message.into(),
            relink: true,
        }))
    }

    pub fn refresh(&self) -> Result<(), Error> {
        let current = self.current.lock().map_err(|_| Error::Screen)?;
        self.send(current.visible())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    fn emotion() -> UiState {
        UiState::Emotion {
            name: "happy".into(),
            frame_ms: 80,
            frames: vec!["(^_^)".into(), "(^o^)".into()],
        }
    }

    #[tokio::test(start_paused = true)]
    async fn m6_animation_expires_at_equality_without_overwriting_current_activity() {
        let (send, mut receive) = mpsc::channel(16);
        let screen = Presentation::new(Some(send));
        screen.publish(UiState::Listening).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_millis(160);
        screen.emotion(emotion(), deadline).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        assert_eq!(receive.try_recv().unwrap(), emotion());
        tokio::time::advance(std::time::Duration::from_millis(159)).await;
        screen.expire_emotion().unwrap();
        assert!(receive.try_recv().is_err());
        screen.emotion_deadline().await;
        assert_eq!(Instant::now(), deadline);
        screen.expire_emotion().unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        screen.expire_emotion().unwrap();
        assert!(receive.try_recv().is_err());
        screen
            .emotion(emotion(), Instant::now() + std::time::Duration::from_millis(160))
            .unwrap();
        screen.publish(UiState::Speaking).unwrap();
        tokio::time::advance(std::time::Duration::from_millis(160)).await;
        screen.expire_emotion().unwrap();
        assert_eq!(receive.try_recv().unwrap(), emotion());
        assert_eq!(receive.try_recv().unwrap(), UiState::Speaking);
        assert!(receive.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn m6_mute_and_mcp_overlays_discard_animation_without_replay() {
        let (send, mut receive) = mpsc::channel(32);
        let screen = Presentation::new(Some(send));
        let deadline = Instant::now() + std::time::Duration::from_millis(160);
        let prompt = UiState::Error {
            message: RELINK.into(),
            relink: true,
        };
        screen.publish(UiState::Listening).unwrap();
        screen.emotion(emotion(), deadline).unwrap();
        screen.overlay(Some(prompt.clone())).unwrap();
        screen.emotion(emotion(), deadline).unwrap();
        screen.publish(UiState::Speaking).unwrap();
        screen.overlay(None).unwrap();
        screen.emotion(emotion(), deadline).unwrap();
        screen.muted(true).unwrap();
        screen.emotion(emotion(), deadline).unwrap();
        screen.muted(false).unwrap();
        tokio::time::advance(std::time::Duration::from_millis(160)).await;
        screen.expire_emotion().unwrap();
        screen.clear_emotion().unwrap();
        let actual: Vec<_> = std::iter::from_fn(|| receive.try_recv().ok()).collect();
        assert_eq!(
            actual,
            vec![
                UiState::Listening,
                emotion(),
                prompt.clone(),
                prompt,
                UiState::Speaking,
                emotion(),
                UiState::Muted,
                UiState::Ready {
                    message: "Echo Live ready".into()
                }
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn m6_cancel_and_replacement_clear_timer_and_queue_failures_stay_bounded() {
        let (send, mut receive) = mpsc::channel(1);
        let screen = Presentation::new(Some(send));
        screen.publish(UiState::Listening).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_millis(160);
        assert_eq!(screen.emotion(emotion(), deadline), Err(Error::Screen));
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        screen.clear_emotion().unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        screen.emotion(emotion(), deadline).unwrap();
        assert_eq!(receive.try_recv().unwrap(), emotion());
        screen.clear_emotion().unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        screen.publish(UiState::Thinking).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Thinking);
        tokio::time::advance(std::time::Duration::from_millis(160)).await;
        screen.expire_emotion().unwrap();
        assert!(receive.try_recv().is_err());
        drop(receive);
        assert_eq!(screen.emotion(emotion(), deadline), Err(Error::Screen));
    }

    #[test]
    fn unmute_emits_only_restored_idle_or_the_active_overlay() {
        let ready = UiState::Ready {
            message: "Echo Live ready".into(),
        };
        let error = UiState::Error {
            message: RELINK.into(),
            relink: true,
        };
        for base in [None, Some(UiState::Listening), Some(UiState::Speaking)] {
            for overlay in [None, Some(error.clone())] {
                let (send, mut receive) = mpsc::channel(8);
                let screen = Presentation::new(Some(send));
                if let Some(base) = base.clone() {
                    screen.publish(base.clone()).unwrap();
                    assert_eq!(receive.try_recv().unwrap(), base);
                }
                screen.muted(true).unwrap();
                assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
                screen.overlay(overlay.clone()).unwrap();
                assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
                screen.muted(false).unwrap();
                assert_eq!(receive.try_recv().unwrap(), overlay.unwrap_or(ready.clone()));
                assert!(receive.try_recv().is_err());
                screen.overlay(None).unwrap();
                assert_eq!(receive.try_recv().unwrap(), ready);
                assert!(receive.try_recv().is_err());
            }
        }
    }

    #[test]
    fn mcp_prompts_and_actions_preserve_the_mute_screen_until_unmuted() {
        let (send, mut receive) = mpsc::channel(16);
        let screen = Presentation::new(Some(send));
        let prompt = UiState::LinkCode {
            user_code: "TEST-CODE".into(),
            verification_uri: "http://127.0.0.1:1/approve".into(),
            expires_at_unix: 1800,
        };
        screen.muted(true).unwrap();
        screen.overlay(Some(prompt.clone())).unwrap();
        screen.publish(UiState::Speaking).unwrap();
        screen.refresh().unwrap();
        for _ in 0..4 {
            assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        }
        screen.muted(false).unwrap();
        assert_eq!(receive.try_recv().unwrap(), prompt);
        screen.muted(true).unwrap();
        screen.rejected(RELINK).unwrap();
        screen.refresh().unwrap();
        for _ in 0..3 {
            assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        }
        screen.muted(false).unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            UiState::Error {
                message: RELINK.into(),
                relink: true
            }
        );
        assert!(receive.try_recv().is_err());
    }

    #[test]
    fn mcp_link_prompt_restores_run_state_and_rejection_survives_activity() {
        let (send, mut receive) = mpsc::channel(8);
        let screen = Presentation::new(Some(send));
        let prompt = UiState::LinkCode {
            user_code: "TEST-CODE".into(),
            verification_uri: "http://127.0.0.1:1/approve".into(),
            expires_at_unix: 1800,
        };
        screen.overlay(Some(prompt.clone())).unwrap();
        screen.publish(UiState::Speaking).unwrap();
        screen.overlay(None).unwrap();
        screen.rejected(RELINK).unwrap();
        screen.publish(UiState::Listening).unwrap();
        assert_eq!(receive.try_recv().unwrap(), prompt);
        assert_eq!(receive.try_recv().unwrap(), prompt);
        assert_eq!(receive.try_recv().unwrap(), UiState::Speaking);
        let error = UiState::Error {
            message: RELINK.into(),
            relink: true,
        };
        assert_eq!(receive.try_recv().unwrap(), error);
        assert_eq!(receive.try_recv().unwrap(), error);
    }
    #[tokio::test(start_paused = true)]
    async fn emotion_owner_wakes_after_idle_and_restores_connected_at_exact_deadline() {
        let (send, mut receive) = mpsc::channel(8);
        let screen = Presentation::new(Some(send));
        screen.publish(UiState::Connected).unwrap();
        assert_eq!(receive.recv().await.unwrap(), UiState::Connected);
        let expiry = screen.advance_emotion();
        tokio::pin!(expiry);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(expiry.as_mut().poll(cx).is_pending())).await);
        screen
            .emotion(emotion(), Instant::now() + std::time::Duration::from_millis(160))
            .unwrap();
        assert_eq!(receive.recv().await.unwrap(), emotion());
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(expiry.as_mut().poll(cx).is_pending())).await);
        tokio::time::advance(std::time::Duration::from_millis(159)).await;
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(expiry.as_mut().poll(cx).is_pending())).await);
        assert!(receive.try_recv().is_err());
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        expiry.await.unwrap();
        assert_eq!(receive.recv().await.unwrap(), UiState::Connected);
        assert!(receive.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn emotion_expiry_cannot_overwrite_a_later_mute_or_relink() {
        for muted in [true, false] {
            let (send, mut receive) = mpsc::channel(8);
            let screen = Presentation::new(Some(send));
            screen
                .emotion(emotion(), Instant::now() + std::time::Duration::from_millis(160))
                .unwrap();
            assert_eq!(receive.recv().await.unwrap(), emotion());
            let expiry = screen.advance_emotion();
            tokio::pin!(expiry);
            assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(expiry.as_mut().poll(cx).is_pending())).await);
            let expected = if muted {
                screen.muted(true).unwrap();
                UiState::Muted
            } else {
                screen.rejected(RELINK).unwrap();
                UiState::Error {
                    message: RELINK.into(),
                    relink: true,
                }
            };
            assert_eq!(receive.recv().await.unwrap(), expected);
            tokio::time::advance(std::time::Duration::from_millis(160)).await;
            expiry.await.unwrap();
            assert!(receive.try_recv().is_err());
            screen.refresh().unwrap();
            assert_eq!(receive.recv().await.unwrap(), expected);
        }
    }
}
