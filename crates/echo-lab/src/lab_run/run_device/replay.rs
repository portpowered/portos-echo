//! One budget shared by confirmed preemption and legacy reply-audio loss.
use super::{Artifacts, Device, Driver, Step};
use crate::lab_run::run_report::talkback::Attempt;

type Outcome = (Result<(), String>, Option<&'static str>);

pub(in crate::lab_run) trait Wake {
    fn arm(&mut self) -> Result<(), String>;
    fn poll(&mut self) -> Result<bool, String>;
    fn wait(&mut self, remaining: std::time::Duration) -> Result<bool, String>;
}

#[derive(Default)]
pub(super) struct Say {
    attempts: u8,
    first_try_woke: Option<bool>,
}

impl Say {
    pub(super) fn dispatched(&mut self) {
        self.attempts += 1;
    }

    pub(super) fn reply(&mut self, woke: bool, legacy: bool) {
        if legacy && self.attempts == 1 && self.first_try_woke.is_none() {
            self.first_try_woke = Some(woke);
        }
    }
}

impl Device<'_> {
    pub(super) fn reply_path(&self, index: usize, root: &std::path::Path) -> std::path::PathBuf {
        let attempts = self.say.borrow().attempts;
        if attempts == 0 && self.iteration == 0 {
            root.join(format!("reply-{index}.wav"))
        } else {
            // A retained earlier WAV must never masquerade as a fresh held
            // recording, especially when the replay is preempted.
            root.join(format!(
                "reply-{index}-iteration-{}-attempt-{}.wav",
                self.iteration,
                attempts + 1
            ))
        }
    }

    pub(super) fn execute_say(&mut self, step: &Step, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        self.observed_say(step, index, artifacts, None)
    }

    pub(in crate::lab_run) fn execute_wake(
        &mut self,
        step: &Step,
        index: usize,
        next: Option<&Step>,
        artifacts: &mut Artifacts,
        wake: Option<&mut dyn Wake>,
    ) -> Result<(), String> {
        self.capture_window = matches!(next, Some(Step::Frame(_)));
        self.observed_say(step, index, artifacts, wake)
    }

    pub(super) fn observed_say(
        &mut self,
        step: &Step,
        index: usize,
        artifacts: &mut Artifacts,
        mut wake: Option<&mut dyn Wake>,
    ) -> Result<(), String> {
        if !matches!(step, Step::SayFile(_) | Step::SayText(_)) {
            return self.attempt(step, index, artifacts);
        }
        *self.say.borrow_mut() = Say::default();
        let result = self.replay_say(step, index, artifacts, &mut wake);
        let say = self.say.borrow();
        if say.attempts > 0 {
            artifacts.step_attempts.push(Attempt {
                step: index,
                iteration: self.iteration,
                attempts: say.attempts,
                first_try_woke: say.first_try_woke,
            });
        }
        result
    }

    fn replay_say(
        &mut self,
        step: &Step,
        index: usize,
        artifacts: &mut Artifacts,
        wake: &mut Option<&mut dyn Wake>,
    ) -> Result<(), String> {
        let (first, reason) = self.say_once(step, index, artifacts, wake)?;
        let Some(reason) = reason else { return first };
        artifacts.event(reason, Some(index), None)?;
        let primary = first.err().ok_or("play: missing first-attempt failure")?;
        self.close(artifacts)
            .map_err(|error| format!("attempt 1: {primary}; replay admission: {error}"))?;
        artifacts.event("retry_started", Some(index), None)?;
        let (second, reason) = self
            .say_once(step, index, artifacts, wake)
            .map_err(|error| format!("attempt 1: {primary}; attempt 2: {error}"))?;
        if let Some(reason) = reason.filter(|reason| *reason == "talkback_lost_suspected") {
            artifacts.event(reason, Some(index), None)?;
        }
        second.map_err(|error| format!("attempt 1: {primary}; attempt 2: {error}"))
    }

    fn say_once(
        &mut self,
        step: &Step,
        index: usize,
        artifacts: &mut Artifacts,
        wake: &mut Option<&mut dyn Wake>,
    ) -> Result<Outcome, String> {
        if let Some(wake) = wake.as_deref_mut() {
            wake.arm()?;
        }
        let result = self.observe_attempt(step, index, artifacts, wake);
        if let Some(wake) = wake.as_deref_mut() {
            let positive = match wake.poll() {
                Ok(positive) => positive,
                Err(error) => return Ok((crate::lab_run::combine(result, Err(error)), None)),
            };
            if positive {
                self.say.borrow_mut().reply(true, true);
                return Ok((result, None));
            }
            if result.is_ok() || result.as_ref().err().map(String::as_str) == Some("play: no post-playback reply audio")
            {
                let end = self
                    .playback_end
                    .borrow()
                    .pending
                    .clone()
                    .ok_or("play: missing playback end")?;
                let remaining = self.host("wake_remaining", &[end])?["remaining_ms"]
                    .as_u64()
                    .ok_or("play: invalid wake deadline")?;
                let remaining = std::time::Duration::from_millis(remaining);
                if remaining
                    > self
                        .clock
                        .deadline(false)
                        .saturating_duration_since(std::time::Instant::now())
                {
                    return Err("play: insufficient lease time for wake observation".into());
                }
                if wake.wait(remaining)? {
                    self.say.borrow_mut().reply(true, true);
                    return Ok((result, None));
                }
                self.say.borrow_mut().reply(false, true);
                return Ok((Err("play: no wake observed".into()), Some("talkback_lost_suspected")));
            }
        }
        let reason = self.retry_reason(&result)?;
        Ok((result, reason))
    }

    fn observe_attempt(
        &mut self,
        step: &Step,
        index: usize,
        artifacts: &mut Artifacts,
        wake: &mut Option<&mut dyn Wake>,
    ) -> Result<(), String> {
        let Some(wake) = wake.as_deref_mut() else {
            return self.attempt(step, index, artifacts);
        };
        // Keep the runtime as the only trace reader while the synchronous bench
        // owns held playback/capture. A wake during capture stays latched.
        std::thread::scope(|scope| {
            let work = scope.spawn(|| self.attempt(step, index, artifacts));
            let mut observation = Ok(());
            while !work.is_finished() {
                if observation.is_ok() {
                    observation = wake.poll().map(|_| ());
                }
                std::thread::park_timeout(std::time::Duration::from_millis(10));
            }
            let result = work.join().map_err(|_| "play: bench worker failed")?;
            crate::lab_run::combine(result, observation)
        })
    }

    fn retry_reason(&self, result: &Result<(), String>) -> Result<Option<&'static str>, String> {
        match result.as_ref().err().map(String::as_str) {
            Some("play: preempted") => Ok(Some("preempted")),
            Some("play: no post-playback reply audio")
                if self.options.bridge_config.is_none() && !self.capture_window =>
            {
                let end = self
                    .playback_end
                    .borrow()
                    .pending
                    .clone()
                    .ok_or("play: missing playback end")?;
                let result = self.host("wake_wait", &[end])?;
                if result["reached"] != true {
                    return Err("play: wake deadline not reached".into());
                }
                self.say.borrow_mut().reply(false, true);
                Ok(Some("talkback_lost_suspected"))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_replay_accounting_preserves_first_verdict_across_second_dispatch() {
        let mut say = Say::default();
        assert_eq!(say.attempts, 0);
        say.dispatched();
        say.reply(false, true);
        say.dispatched();
        say.reply(true, true);
        assert_eq!(say.attempts, 2);
        assert_eq!(say.first_try_woke, Some(false));
    }

    #[test]
    fn ring_replay_accounting_never_infers_configured_wake_from_reply() {
        let mut say = Say::default();
        say.dispatched();
        say.reply(true, false);
        assert_eq!(say.first_try_woke, None);
        say.dispatched();
        say.reply(true, true);
        assert_eq!(say.first_try_woke, None);
    }
}
