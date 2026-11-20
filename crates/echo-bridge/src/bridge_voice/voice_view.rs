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

pub(super) struct View<'a> {
    screen: &'a Screen,
    health: &'a Health,
    trace: &'a mut Trace,
    sequence: u64,
    run: Option<String>,
    connected: bool,
    pending_connected: bool,
    mouth_paused: bool,
    startup: Startup,
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
            mouth_paused: false,
            startup: Startup::Pending,
        }
    }

    pub fn observe(&mut self, notice: ServeNotice) -> Result<(), Error> {
        match notice {
            ServeNotice::Kill(enabled) => self.kill(enabled),
            ServeNotice::Ready => {
                self.startup = Startup::Ready;
                self.closed("idle")?;
                self.relinked()?;
                self.health.state.send_modify(|state| {
                    state.wyoming = "connected";
                    state.startup_waiting_reason = None;
                });
                self.screen.ready()?;
                self.trace.record("bridge.ready", None, json!({}))?;
                crate::bridge_runtime::publish("bridge.ready", None)
            }
            ServeNotice::Listening => self.begin(),
            ServeNotice::Unanswered => {
                let run = self.run.take();
                self.trace
                    .record("run.ended", run.as_deref(), json!({"reason":"no_response"}))?;
                self.closed("no_response")
            }
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
        self.mouth_paused = false;
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
            SessionNotice::Run(Notice::Device(event)) => self.screen.observe(&event, self.trace).map(|_| ()),
            SessionNotice::Run(Notice::EarconPlayed(duration)) => {
                self.trace.record(
                    "wake.earcon_played",
                    self.run.as_deref(),
                    json!({"duration_ms":duration.as_millis()}),
                )?;
                if std::mem::take(&mut self.pending_connected) {
                    self.screen.publish(UiState::Connected)?;
                }
                Ok(())
            }
            SessionNotice::Run(Notice::ReplyStarted) => {
                self.mouth_paused = false;
                self.screen.publish(UiState::Speaking)
            }
            SessionNotice::Run(Notice::ReplyPaused) => {
                self.mouth_paused = true;
                self.screen.ready()
            }
            SessionNotice::Run(Notice::SessionTool(receipt)) => {
                let mut fields = json!({"tool":receipt.tool,"outcome":receipt.outcome});
                if let (Some(deadline), Some(capped)) = (receipt.deadline_ms, receipt.capped) {
                    fields["deadline_ms"] = deadline.into();
                    fields["capped"] = capped.into();
                }
                self.trace.record("session.tool_receipt", self.run.as_deref(), fields)
            }
            SessionNotice::Run(Notice::ReplyPlayed | Notice::ReplySettled(_) | Notice::Transcript { .. }) => Ok(()),
            SessionNotice::MicrophoneCompleted(stats) => self.microphone_completed(&stats),
            SessionNotice::ReplyCompleted(stats) => self.reply_completed(&stats),
            SessionNotice::Completed(_) => self.completed(),
        }
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
        if !self.mouth_paused {
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
                    "cut_ms":cut.map(|cut| cut.yield_ms), "text":cut.map(|cut| cut.text.as_str())},
                "mic":{"onsets":quality.mic.onsets, "echo_max":quality.mic.echo_max,
                    "level_max":quality.mic.level_max, "coupling":quality.mic.coupling,
                    "echo_mean":quality.mic.echo_mean, "noise":quality.mic.noise},
                "aec":aec(quality.aec)
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
        self.mouth_paused = false;
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
        let state = crate::bridge_runtime::bridge_screen::failure_state(relink);
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

/// What the echo canceller measured under one reply; null while it is off.
fn aec(measured: Option<super::Cancelled>) -> serde_json::Value {
    let Some(aec) = measured else {
        return serde_json::Value::Null;
    };
    json!({"delay_ms":aec.delay_ms, "erle_db":f64::from(aec.erle_db10) / 10.0, "converged":aec.converged})
}

#[cfg(test)]
#[path = "earcon/voice_view_tests.rs"]
mod tests;
