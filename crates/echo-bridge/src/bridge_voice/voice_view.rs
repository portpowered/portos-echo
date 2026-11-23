//! Project only service facts, never transcript bodies or provider payloads.
use super::{BargeTrigger, Notice, ServeNotice, SessionNotice, Stats};
use crate::{Error, bridge_health::Health, bridge_runtime::bridge_screen::Screen, bridge_trace::Trace};
use echo_screen_contract::UiState;
use serde_json::json;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Startup {
    Pending,
    Ready,
}

/// What the reply-driven publications last showed. Within a run the face
/// keeps its mouth: closed (Connected) whenever reply audio is not playing.
/// Only the run's end, or a pause with no run, returns the screen to Ready.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MouthActivity {
    AwaitingReady,
    Speaking,
    /// Connected after a reply's audio stopped; the run is still active.
    Closed,
    /// Ready was published for a pause outside any run.
    Paused,
    Thinking,
    ThinkingAfterEarcon,
}

pub(super) struct View<'a> {
    screen: &'a Screen,
    health: &'a Health,
    trace: &'a mut Trace,
    sequence: u64,
    run: Option<String>,
    connected: bool,
    pending_connected: bool,
    mouth: MouthActivity,
    startup: Startup,
    account_relink: Option<&'static str>,
}

impl<'a> View<'a> {
    pub(super) fn setup_trace(&self) -> Trace {
        self.trace.clone()
    }

    pub(super) async fn open_tools(
        &mut self,
        tools: &mut super::voice_tools::Tools,
        config: &echo_config::BridgeConfig,
        epoch: Option<u64>,
    ) -> Result<(), Error> {
        tools.open(config, self.screen, self.trace, epoch).await
    }

    pub(super) async fn open_portos(
        &mut self,
        config: &echo_config::BridgeConfig,
        epoch: Option<u64>,
    ) -> Result<Option<crate::bridge_mcp::Runtime>, Error> {
        super::voice_tools::open_portos(config, self.screen, self.trace, epoch).await
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
            pending_connected: false,
            mouth: MouthActivity::AwaitingReady,
            startup: Startup::Pending,
            account_relink: None,
        }
    }

    pub(super) fn with_screen_account(mut self, enabled: bool) -> Self {
        self.account_relink = enabled.then_some("Open Settings and link OpenAI under Accounts.");
        self
    }

    fn failure_state(&self, relink: bool) -> UiState {
        if relink && let Some(message) = self.account_relink {
            UiState::Error {
                message: message.into(),
                relink: true,
            }
        } else {
            crate::bridge_runtime::bridge_screen::failure_state(relink)
        }
    }

    pub fn observe(&mut self, notice: ServeNotice) -> Result<(), Error> {
        match notice {
            ServeNotice::Kill(enabled) => self.kill(enabled),
            ServeNotice::Ready => {
                let trace_witness = self.startup == Startup::Pending
                    && std::env::var_os("A1_TRACE_TICK_WITNESS").is_some();
                self.startup = Startup::Ready;
                self.closed("idle")?;
                self.relinked()?;
                self.health.state.send_modify(|state| {
                    state.wyoming = "connected";
                    state.startup_waiting_reason = None;
                });
                self.screen.ready()?;
                self.trace.record("bridge.ready", None, json!({}))?;
                if trace_witness {
                    self.trace.buffer_clock_offset(json!({"synced":false, "reason":"synthetic-tick"}))?;
                }
                crate::bridge_runtime::publish("bridge.ready", None)
            }
            ServeNotice::Listening => self.begin(),
            ServeNotice::Unanswered { reason } => {
                let run = self.run.take();
                self.trace
                    .record("run.ended", run.as_deref(), json!({"reason":reason}))?;
                self.closed(reason)
            }
            ServeNotice::Probe => self.trace.record("wyoming.probe_ok", Some("connection-1"), json!({})),
            ServeNotice::Device(event) => self.device(&event),
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
        self.mouth = MouthActivity::AwaitingReady;
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Unavailable)?;
        self.run = Some(format!("run-{}", self.sequence));
        self.trace.tool_run(self.run.as_deref())?;
        self.trace
            .record("wyoming.run_started", self.run.as_deref(), json!({}))?;
        self.trace.record("run.started", self.run.as_deref(), json!({}))?;
        if let Some(reason) = crate::bridge_endpoint::remote_start::reason() {
            self.trace.record(
                "endpoint.session_started",
                self.run.as_deref(),
                json!({"reason":reason}),
            )?;
        }
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
                self.pending_connected = super::earcon::active();
                if self.pending_connected {
                    Ok(())
                } else {
                    self.screen.publish(UiState::Connected)
                }
            }
            SessionNotice::Started => {
                // Initial Listening preceded negotiation. Reused calls need
                // their own Listening, without a second Connected publication.
                if self.run.is_none() {
                    self.begin()?;
                }
                Ok(())
            }
            SessionNotice::Run(Notice::Device(event)) => self.device(&event),
            SessionNotice::Run(Notice::EarconPlayed(duration)) => {
                self.trace.record(
                    "wake.earcon_played",
                    self.run.as_deref(),
                    json!({"duration_ms":duration.as_millis()}),
                )?;
                if std::mem::take(&mut self.pending_connected) {
                    self.screen.publish(UiState::Connected)?;
                }
                if self.mouth == MouthActivity::ThinkingAfterEarcon {
                    self.show_thinking()?;
                }
                Ok(())
            }
            SessionNotice::Run(Notice::Thinking) => self.thinking(),
            SessionNotice::Run(Notice::ReplyStarted) => {
                self.mouth = MouthActivity::Speaking;
                self.screen.publish(UiState::Speaking)
            }
            SessionNotice::Run(Notice::ReplyPaused) => self.reply_paused(),
            SessionNotice::Run(Notice::SessionTool(receipt)) => {
                let mut fields = json!({"tool":receipt.tool,"outcome":receipt.outcome});
                if let (Some(deadline), Some(capped)) = (receipt.deadline_ms, receipt.capped) {
                    fields["deadline_ms"] = deadline.into();
                    fields["capped"] = capped.into();
                }
                self.trace.record("session.tool_receipt", self.run.as_deref(), fields)
            }
            SessionNotice::Run(Notice::Mark(mark)) => {
                self.trace.record(mark.event(), self.run.as_deref(), mark.fields())
            }
            SessionNotice::Run(Notice::ReplyPlayed | Notice::ReplySettled(_) | Notice::Transcript { .. }) => Ok(()),
            SessionNotice::MicrophoneCompleted(stats) => self.microphone_completed(&stats),
            SessionNotice::ReplyCompleted(stats) => self.reply_completed(&stats),
            SessionNotice::Completed(_) => self.completed(),
        }
    }

    fn device(&mut self, event: &echo_link::LinkEvent<()>) -> Result<(), Error> {
        if matches!(event, echo_link::LinkEvent::StaleRemoteCapture) {
            return self.trace.record("endpoint.mic_start_stale", None, json!({}));
        }
        if matches!(event, echo_link::LinkEvent::Mute(true)) && self.mouth == MouthActivity::ThinkingAfterEarcon {
            self.mouth = MouthActivity::AwaitingReady;
        }
        self.screen.observe(event, self.trace).map(|_| ())
    }

    /// Reply audio stopped. During a run the face stays awake with a closed
    /// mouth; a reply gap never publishes Ready mid-run.
    fn reply_paused(&mut self) -> Result<(), Error> {
        if self.run.is_none() {
            self.mouth = MouthActivity::Paused;
            return self.screen.ready();
        }
        // Delegated Thinking holds through the gap; muting suppresses activity.
        if matches!(self.mouth, MouthActivity::Thinking | MouthActivity::ThinkingAfterEarcon) || self.screen.muted()? {
            return Ok(());
        }
        self.mouth = MouthActivity::Closed;
        self.screen.publish(UiState::Connected)
    }

    fn thinking(&mut self) -> Result<(), Error> {
        if self.screen.muted()? {
            return Ok(());
        }
        if self.pending_connected || super::earcon::active() {
            self.mouth = MouthActivity::ThinkingAfterEarcon;
            Ok(())
        } else {
            self.show_thinking()
        }
    }

    fn show_thinking(&mut self) -> Result<(), Error> {
        self.mouth = MouthActivity::Thinking;
        self.screen.presentation.thinking()
    }

    fn microphone_completed(&mut self, stats: &Stats) -> Result<(), Error> {
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
        if stats.uplink_dropped > 0 {
            // Speech that never reached the call is a named loss, never silent.
            self.trace.record(
                "mic.uplink_dropped",
                Some(id),
                json!({"count":stats.uplink_dropped, "reason":"backlog_full"}),
            )?;
        }
        Ok(())
    }

    fn completed(&mut self) -> Result<(), Error> {
        let id = self.run.as_deref().ok_or(Error::Unavailable)?;
        self.trace.record("wyoming.run_ended", Some(id), json!({}))?;
        self.health.state.send_modify(|state| state.live = "idle");
        if self.mouth != MouthActivity::Paused {
            self.screen.ready()?;
        }
        self.trace.record("bridge.run_completed", Some(id), json!({}))?;
        crate::bridge_runtime::publish("bridge.run_completed", Some(id))?;
        self.run = None;
        self.trace.tool_run(None)?;
        Ok(())
    }

    fn reply_completed(&mut self, stats: &Stats) -> Result<(), Error> {
        let id = self.run.as_deref().ok_or(Error::Unavailable)?;
        let (quality, cut) = (&stats.quality, stats.cut.as_deref());
        self.trace.record(
            "reply.played",
            Some(id),
            json!({"outcome":if quality.cuts > 0 { "cut" } else { "played" },
                "frames":stats.reply_sent, "received_frames":stats.reply_received, "dropped_frames":stats.reply_dropped}),
        )?;
        // Measurements, not journey steps: they vary with timing run to run.
        self.trace.record(
            "reply.quality",
            Some(id),
            json!({"arrival_gap_max_ms":quality.arrival_gap_max_ms, "late_arrivals":quality.late_arrivals,
                "underruns":quality.underruns, "underrun_max_ms":quality.underrun_max_ms,
                "depth_min_ms":quality.depth_min_ms, "depth_max_ms":quality.depth_max_ms,
                "discarded":{"after_reply":quality.discarded_after_reply, "idle":quality.discarded_idle,
                    "cut":quality.discarded_cut, "audible":quality.discarded_audible},
                "rtp_lost":quality.rtp_lost,
                "target_ms":{"min":quality.target_min_ms, "max":quality.target_max_ms,
                    "final":quality.target_final_ms},
                "reprimes":quality.reprimes, "concealment_ms":quality.concealment_ms,
                "fade_ins":quality.fade_ins, "drained_frames":quality.drained_frames,
                "cuts":{"count":quality.cuts, "trigger":quality.cut_trigger.map(BargeTrigger::name),
                    "holds":quality.holds, "hold_ms":quality.hold_ms, "silent_ms":cut.map(|cut| cut.silent_ms),
                    "cut_ms":cut.map(|cut| cut.yield_ms),
                    "text":cut.filter(|_| !crate::bridge_endpoint::remote_start::remote()).map(|cut| cut.text.as_str())},
                "mic":{"onsets":quality.mic.onsets, "echo_max":quality.mic.echo_max,
                    "level_max":quality.mic.level_max, "coupling":quality.mic.coupling,
                    "echo_mean":quality.mic.echo_mean, "noise":quality.mic.noise},
                "lips":{"batches":quality.lips.batches, "samples":quality.lips.samples,
                    "dropped":quality.lips.dropped, "publish_max_ms":quality.lips.publish_max_ms,
                    "lead_min_ms":quality.lips.lead_min_ms, "late":quality.lips.late}
            }),
        )?;
        let Some(first) = quality.first_audio else {
            return Ok(());
        };
        // The latency budget's sample: endpoint to the first reply frame written.
        self.trace.record(
            "timing",
            Some(id),
            json!({"name":"first_audio_after_endpoint_ms",
                "duration_ms":first.after_endpoint_ms, "endpoint":first.endpoint.name()}),
        )
    }

    pub fn closed(&mut self, reason: &'static str) -> Result<(), Error> {
        self.pending_connected = false;
        self.mouth = MouthActivity::AwaitingReady;
        self.screen.presentation.clear_thinking()?;
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

    /// A recovered generation withdraws a relink instruction it no longer needs.
    fn relinked(&mut self) -> Result<(), Error> {
        // Health is the one record of a relink this view asked for.
        if self.health.state.borrow().relink_required {
            self.health.state.send_modify(|state| state.relink_required = false);
            if !self.health.state.borrow().kill_switch {
                self.screen.presentation.overlay(None)?;
            }
        }
        Ok(())
    }

    /// A failure that will be retried. Health and screen show what a terminal
    /// failure shows; the trace names the retry instead of `bridge.error`.
    pub fn recovering(&mut self, error: &Error, retry: (u32, std::time::Duration), linked: bool) -> Result<(), Error> {
        let run = self.run.take();
        self.closed("failed")?;
        let relink = *error == Error::Authorization;
        if !relink {
            self.relinked()?;
        }
        self.health.state.send_modify(|state| {
            state.live = "error";
            state.relink_required = relink;
            state.startup_waiting_reason =
                (self.startup == Startup::Pending && !relink).then_some("dependency_unavailable");
            if !linked {
                state.wyoming = "connecting";
            }
        });
        let state = self.failure_state(relink);
        if relink {
            self.screen.presentation.overlay(Some(state))?;
        } else {
            self.screen.publish(state)?;
        }
        self.trace.record(
            "bridge.recovering",
            run.as_deref(),
            json!({"reason":error.to_string(), "attempt":retry.0, "delay_ms":retry.1.as_millis()}),
        )
    }

    pub async fn failed(&mut self, error: &Error) -> Result<(), Error> {
        self.closed("failed")?;
        let relink = *error == Error::Authorization;
        self.health.state.send_modify(|state| {
            state.live = "error";
            state.relink_required = relink;
            state.startup_waiting_reason = None;
        });
        let state = self.failure_state(relink);
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
#[path = "earcon/voice_view_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "earcon/voice_reply_hold_tests.rs"]
mod reply_hold_tests;

#[cfg(test)]
mod thinking_tests {
    use super::*;
    use crate::bridge_mcp::bridge_presentation::Presentation;
    use tokio::sync::mpsc;

    #[tokio::test(start_paused = true)]
    async fn thinking_preserves_prompt_and_relink_then_resets_for_a_direct_reply() {
        let directory = tempfile::tempdir().unwrap();
        let mut trace = Trace::open(&directory.path().join("trace"), "chatgpt").unwrap();
        let health = Health::offline();
        let (send, mut receive) = mpsc::channel(8);
        let mut screen = Screen::bind(None).await.unwrap();
        screen.presentation = Presentation::new(Some(send));
        let mut view = View::new(&screen, &health, &mut trace);
        view.observe(ServeNotice::Listening).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        let prompt = UiState::Approval {
            id: "local".into(),
            prompt: "Proceed?".into(),
            accept_label: "Proceed".into(),
            deny_label: "Cancel".into(),
        };
        screen.presentation.approval(prompt.clone()).unwrap();
        assert_eq!(receive.try_recv().unwrap(), prompt);
        for _ in 0..2 {
            view.session(SessionNotice::Run(Notice::Thinking)).unwrap();
            assert!(receive.try_recv().is_err());
        }
        screen.presentation.clear_approval("local").unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Thinking);
        view.session(SessionNotice::Run(Notice::Thinking)).unwrap();
        assert!(receive.try_recv().is_err());
        view.recovering(&Error::Authorization, (1, std::time::Duration::from_secs(1)), true)
            .unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            crate::bridge_runtime::bridge_screen::failure_state(true)
        );
        view.observe(ServeNotice::Ready).unwrap();
        let ready = UiState::Ready {
            message: "Echo Live ready".into(),
        };
        assert_eq!(receive.try_recv().unwrap(), ready);
        assert_eq!(receive.try_recv().unwrap(), ready);
        view.observe(ServeNotice::Listening).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        tests::complete(&mut view, &mut receive);
    }
}
