//! Project only service facts, never transcript bodies or provider payloads.
use super::{Notice, ServeNotice, SessionNotice, Stats};
use crate::{Error, bridge_health::Health, bridge_runtime::bridge_screen::Screen, bridge_trace::Trace};
use echo_screen_contract::UiState;
use serde_json::json;

pub(super) struct View<'a> {
    screen: &'a Screen,
    health: &'a Health,
    trace: &'a mut Trace,
    sequence: u64,
    run: Option<String>,
    connected: bool,
}

impl<'a> View<'a> {
    pub(super) async fn open_tools(
        &mut self,
        tools: &mut super::voice_tools::Tools,
        config: &echo_config::BridgeConfig,
    ) -> Result<(), Error> {
        tools.open(config, self.screen, self.trace).await
    }

    pub fn startup(&mut self) -> Result<(), Error> {
        self.trace.record(
            "bridge.started",
            None,
            json!({"health_addr":self.health.address.to_string()}),
        )?;
        if let Some(address) = self.screen.address() {
            self.trace
                .record("screen.listening", None, json!({"address":address.to_string()}))?;
        }
        Ok(())
    }

    pub fn new(screen: &'a Screen, health: &'a Health, trace: &'a mut Trace) -> Self {
        Self {
            screen,
            health,
            trace,
            sequence: 0,
            run: None,
            connected: false,
        }
    }

    pub fn observe(&mut self, notice: ServeNotice) -> Result<(), Error> {
        match notice {
            ServeNotice::Kill(enabled) => self.kill(enabled),
            ServeNotice::Ready => {
                self.closed("idle")?;
                self.health.state.send_modify(|state| state.wyoming = "connected");
                self.screen.ready()?;
                self.trace.record("bridge.ready", None, json!({}))?;
                crate::bridge_runtime::publish("bridge.ready", None)
            }
            ServeNotice::Listening => self.begin(),
            ServeNotice::Probe => self.trace.record("wyoming.probe_ok", Some("connection-1"), json!({})),
            ServeNotice::Device(event) => self.screen.observe(&event, self.trace).map(|_| ()),
            ServeNotice::Session(notice) => self.session(notice),
        }
    }

    fn kill(&mut self, enabled: bool) -> Result<(), Error> {
        if enabled {
            self.closed("kill")?;
            self.run = None;
        }
        self.health.state.send_modify(|state| {
            state.kill_switch = enabled;
            state.live = if enabled { "error" } else { "idle" };
        });
        self.screen.presentation.overlay(enabled.then(|| UiState::Error {
            message: "Kill switch enabled. Run echo-bridge kill --clear with your config.".into(),
            relink: false,
        }))?;
        self.trace
            .record(if enabled { "kill.enabled" } else { "kill.cleared" }, None, json!({}))
    }

    fn begin(&mut self) -> Result<(), Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Unavailable)?;
        self.run = Some(format!("run-{}", self.sequence));
        self.trace.tool_run(self.run.as_deref())?;
        self.trace
            .record("wyoming.run_started", self.run.as_deref(), json!({}))?;
        self.trace.record("run.started", self.run.as_deref(), json!({}))?;
        self.health
            .state
            .send_modify(|state| state.live = if self.connected { "connected" } else { "connecting" });
        self.screen.publish(UiState::Listening)
    }

    fn session(&mut self, notice: SessionNotice) -> Result<(), Error> {
        match notice {
            SessionNotice::Connected => {
                self.connected = true;
                self.health.state.send_modify(|state| state.live = "connected");
                self.screen.publish(UiState::Connected)
            }
            SessionNotice::Started => {
                // Initial Listening preceded negotiation. Reused calls need
                // their own Listening, without a second Connected publication.
                if self.run.is_none() {
                    self.begin()?;
                }
                Ok(())
            }
            SessionNotice::Run(Notice::Device(event)) => self.screen.observe(&event, self.trace).map(|_| ()),
            SessionNotice::Run(Notice::ReplyStarted) => self.screen.publish(UiState::Speaking),
            SessionNotice::Run(Notice::ReplyPlayed | Notice::Transcript { .. }) => Ok(()),
            SessionNotice::Completed(stats) => self.completed(&stats),
        }
    }

    fn completed(&mut self, stats: &Stats) -> Result<(), Error> {
        let id = self.run.as_deref().ok_or(Error::Unavailable)?;
        self.trace
            .record("wyoming.mic_frames", Some(id), json!({"count":stats.received_chunks}))?;
        self.trace.record(
            "mic.completed",
            Some(id),
            json!({
                "count":stats.microphone_frames, "received_chunks":stats.received_chunks,
                "dropped_chunks":stats.dropped_chunks, "tail_samples":stats.microphone_tail
            }),
        )?;
        self.trace.record(
            "reply.played",
            Some(id),
            json!({"outcome":"played",
                "frames":stats.reply_sent, "received_frames":stats.reply_received, "dropped_frames":stats.reply_dropped
            }),
        )?;
        self.trace.record("wyoming.run_ended", Some(id), json!({}))?;
        self.health.state.send_modify(|state| state.live = "idle");
        self.screen.ready()?;
        self.trace.record("bridge.run_completed", Some(id), json!({}))?;
        crate::bridge_runtime::publish("bridge.run_completed", Some(id))?;
        self.run = None;
        self.trace.tool_run(None)?;
        Ok(())
    }

    pub fn closed(&mut self, reason: &'static str) -> Result<(), Error> {
        self.trace.tool_run(None)?;
        if self.connected {
            self.trace.record("live.closed", None, json!({"reason":reason}))?;
            self.connected = false;
        }
        self.health
            .state
            .send_modify(|state| state.live = if state.kill_switch { "error" } else { "idle" });
        Ok(())
    }

    pub async fn failed(&mut self, error: &Error) -> Result<(), Error> {
        self.closed("failed")?;
        let relink = *error == Error::Authorization;
        self.health.state.send_modify(|state| {
            state.live = "error";
            state.relink_required = relink;
        });
        let state = crate::bridge_runtime::bridge_screen::failure_state(relink);
        if relink {
            self.screen.presentation.overlay(Some(state))?;
            self.screen.flush().await?;
        } else {
            self.screen.publish_confirmed(state).await?;
        }
        self.trace.record(
            if relink { "auth.relink_required" } else { "bridge.error" },
            self.run.as_deref(),
            json!({"reason":error.to_string()}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_mcp::bridge_presentation::Presentation;
    use echo_link::LinkEvent;
    use tokio::sync::mpsc;

    fn rows(path: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn killed_overlay_survives_mute_and_ready_and_clear_restores_idle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "chatgpt").unwrap();
        let health = Health::offline();
        let (send, mut receive) = mpsc::channel(16);
        let mut screen = Screen::bind(None).await.unwrap();
        screen.presentation = Presentation::new(Some(send));
        let mut view = View::new(&screen, &health, &mut trace);
        view.observe(ServeNotice::Listening).unwrap();
        view.observe(ServeNotice::Session(SessionNotice::Connected)).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        assert_eq!(receive.try_recv().unwrap(), UiState::Connected);
        view.observe(ServeNotice::Kill(true)).unwrap();
        let killed = receive.try_recv().unwrap();
        assert_eq!(
            killed,
            UiState::Error {
                message: "Kill switch enabled. Run echo-bridge kill --clear with your config.".into(),
                relink: false,
            }
        );
        view.observe(ServeNotice::Ready).unwrap();
        assert_eq!(receive.try_recv().unwrap(), killed);
        assert!(health.state.borrow().kill_switch);
        assert_eq!(health.state.borrow().live, "error");
        view.observe(ServeNotice::Device(LinkEvent::Mute(true))).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        view.observe(ServeNotice::Device(LinkEvent::Mute(false))).unwrap();
        assert_eq!(receive.try_recv().unwrap(), killed);
        view.observe(ServeNotice::Kill(false)).unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            UiState::Ready {
                message: "Echo Live ready".into()
            }
        );
        assert!(!health.state.borrow().kill_switch);
        assert_eq!(health.state.borrow().live, "idle");
        let rows = rows(&path);
        assert_eq!(rows.iter().filter(|row| row["event"] == "live.closed").count(), 1);
        assert_eq!(
            rows.iter().find(|row| row["event"] == "live.closed").unwrap()["reason"],
            "kill"
        );
        for event in ["kill.enabled", "kill.cleared"] {
            assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), 1);
        }
        assert_eq!(
            rows.iter().filter(|row| row["event"] == "bridge.run_completed").count(),
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn projection_distinguishes_negotiation_played_reuse_and_idle_stop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "chatgpt").unwrap();
        let health = Health::offline();
        let (send, mut receive) = mpsc::channel(16);
        let mut screen = Screen::bind(None).await.unwrap();
        screen.presentation = Presentation::new(Some(send));
        let mut view = View::new(&screen, &health, &mut trace);
        view.observe(ServeNotice::Ready).unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            UiState::Ready {
                message: "Echo Live ready".into()
            }
        );
        assert_eq!(health.state.borrow().live, "idle");
        view.observe(ServeNotice::Listening).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        assert_eq!(health.state.borrow().live, "connecting");
        view.observe(ServeNotice::Session(SessionNotice::Connected)).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Connected);
        assert_eq!(health.state.borrow().live, "connected");
        view.observe(ServeNotice::Session(SessionNotice::Started)).unwrap();
        assert!(receive.try_recv().is_err());
        for run in 1..=2 {
            complete(&mut view, &mut receive);
            assert_eq!(health.state.borrow().live, "idle");
            assert_eq!(
                rows(&path)
                    .iter()
                    .filter(|row| row["event"] == "bridge.run_completed")
                    .count(),
                run
            );
            if run == 1 {
                view.observe(ServeNotice::Session(SessionNotice::Started)).unwrap();
                assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
                assert_eq!(health.state.borrow().live, "connected");
            }
        }
        view.observe(ServeNotice::Ready).unwrap();
        view.closed("shutdown").unwrap();
        completed_trace(&path);
    }

    fn completed_trace(path: &std::path::Path) {
        let rows = rows(path);
        assert_eq!(rows.iter().filter(|row| row["event"] == "live.closed").count(), 1);
        assert_eq!(
            rows.iter().find(|row| row["event"] == "live.closed").unwrap()["reason"],
            "idle"
        );
        for (index, row) in rows
            .iter()
            .filter(|row| row["event"] == "bridge.run_completed")
            .enumerate()
        {
            assert_eq!(row["run_id"], format!("run-{}", index + 1));
        }
        for played in rows.iter().filter(|row| row["event"] == "reply.played") {
            assert_eq!(played["outcome"], "played");
            assert_eq!(played["frames"], 8);
            assert_eq!(played["received_frames"], 9);
            assert_eq!(played["dropped_frames"], 1);
        }
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("private transcript fixture")
        );
    }

    fn complete(view: &mut View<'_>, receive: &mut mpsc::Receiver<UiState>) {
        view.observe(ServeNotice::Session(SessionNotice::Run(Notice::ReplyStarted)))
            .unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Speaking);
        for notice in [
            Notice::Transcript {
                role: "assistant".into(),
                text: "private transcript fixture".into(),
                done: true,
            },
            Notice::ReplyPlayed,
        ] {
            view.observe(ServeNotice::Session(SessionNotice::Run(notice))).unwrap();
            assert!(receive.try_recv().is_err());
        }
        view.observe(ServeNotice::Session(SessionNotice::Completed(Stats {
            received_chunks: 7,
            microphone_frames: 3,
            reply_received: 9,
            reply_sent: 8,
            reply_dropped: 1,
            ..Stats::default()
        })))
        .unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            UiState::Ready {
                message: "Echo Live ready".into()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn tool_receipts_follow_reused_wakes_and_never_keep_a_finished_run() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "chatgpt").unwrap();
        let mut effect = trace.clone();
        let health = Health::offline();
        let screen = Screen::bind(None).await.unwrap();
        let mut view = View::new(&screen, &health, &mut trace);
        view.observe(ServeNotice::Listening).unwrap();
        effect.tool_attempt("test.echo", "1:t1:u1:c").unwrap();
        view.observe(ServeNotice::Session(SessionNotice::Completed(Stats::default())))
            .unwrap();
        effect.tool_attempt("test.echo", "1:t1:u1:d").unwrap();
        view.observe(ServeNotice::Session(SessionNotice::Started)).unwrap();
        effect.tool_attempt("test.echo", "1:t1:v1:c").unwrap();
        view.observe(ServeNotice::Kill(true)).unwrap();
        effect.tool_attempt("test.echo", "1:t1:v1:d").unwrap();
        let runs: Vec<_> = rows(&path)
            .into_iter()
            .filter(|row| row["event"] == "tools/call")
            .map(|row| row["run_id"].clone())
            .collect();
        assert_eq!(runs, vec![json!("run-1"), json!(null), json!("run-2"), json!(null)]);
    }

    #[tokio::test(start_paused = true)]
    async fn completed_state_queue_failure_never_claims_run_completion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "chatgpt").unwrap();
        let health = Health::offline();
        let (send, mut receive) = mpsc::channel(1);
        let mut screen = Screen::bind(None).await.unwrap();
        screen.presentation = Presentation::new(Some(send));
        let mut view = View::new(&screen, &health, &mut trace);
        view.observe(ServeNotice::Listening).unwrap();
        assert_eq!(
            view.observe(ServeNotice::Session(SessionNotice::Completed(Stats::default()))),
            Err(Error::Screen)
        );
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        assert_eq!(
            rows(&path)
                .iter()
                .filter(|row| row["event"] == "bridge.run_completed")
                .count(),
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn projection_privacy_relink_and_publication_failure_preserve_honest_health() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "chatgpt").unwrap();
        let health = Health::offline();
        let (send, mut receive) = mpsc::channel(1);
        let mut screen = Screen::bind(None).await.unwrap();
        screen.presentation = Presentation::new(Some(send));
        let mut view = View::new(&screen, &health, &mut trace);
        view.observe(ServeNotice::Device(LinkEvent::Mute(true))).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        view.observe(ServeNotice::Device(LinkEvent::MicrophoneDropped(4)))
            .unwrap();
        view.observe(ServeNotice::Session(SessionNotice::Run(Notice::Device(
            LinkEvent::MicrophoneDropped(1),
        ))))
        .unwrap();
        view.observe(ServeNotice::Session(SessionNotice::Run(Notice::ReplyStarted)))
            .unwrap();
        assert!(receive.try_recv().is_err());
        view.failed(&Error::Authorization).await.unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        assert_eq!(health.state.borrow().live, "error");
        assert!(health.state.borrow().relink_required);
        view.observe(ServeNotice::Device(LinkEvent::Mute(false))).unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            crate::bridge_runtime::bridge_screen::failure_state(true)
        );
        view.failed(&Error::Authorization).await.unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            crate::bridge_runtime::bridge_screen::failure_state(true)
        );
        let windows: Vec<_> = rows(&path)
            .into_iter()
            .filter(|row| row["event"] == "mic.mute_window")
            .collect();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0]["received_frames"], 5);
        assert_eq!(windows[0]["forwarded_frames"], 0);
        screen.presentation.refresh().unwrap();
        assert_eq!(view.failed(&Error::Authorization).await, Err(Error::Screen));
        assert_eq!(
            rows(&path)
                .iter()
                .filter(|row| row["event"] == "auth.relink_required")
                .count(),
            2
        );
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("private transcript fixture")
        );
    }
}
