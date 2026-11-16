//! Wire-ordered privacy measurements, including chunks received without a run.
use crate::{Error, bridge_trace::Trace};
use echo_link::LinkEvent;
use echo_screen_contract::UiState;
use serde_json::json;

#[derive(Default)]
pub(super) struct Mute {
    pub muted: bool,
    windows: u64,
    received: u64,
    forwarded: u64,
}

impl Mute {
    pub fn event<R>(&mut self, event: &LinkEvent<R>, trace: &mut Trace) -> Result<bool, Error> {
        if let LinkEvent::MicrophoneDropped(count) = event
            && self.muted
        {
            self.received += count;
        }
        if let LinkEvent::Mute(muted) = event
            && *muted != self.muted
        {
            self.transition(*muted, trace)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn transition(&mut self, muted: bool, trace: &mut Trace) -> Result<(), Error> {
        if muted {
            self.windows += 1;
            self.received = 0;
            self.forwarded = 0;
        } else {
            trace.record(
                "mic.mute_window",
                None,
                json!({"window_id":format!("mute-{}", self.windows),
                    "received_frames":self.received,"forwarded_frames":self.forwarded}),
            )?;
        }
        self.muted = muted;
        Ok(())
    }

    pub fn received(&mut self) {
        if self.muted {
            self.received += 1;
        }
    }

    pub fn sent(&mut self) {
        if self.muted {
            self.forwarded += 1;
        }
    }

    pub fn project(&self, state: UiState) -> UiState {
        if self.muted { UiState::Muted } else { state }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn neutral_mute_windows_ignore_duplicates_and_guard_all_activity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        let mut mute = Mute::default();
        for state in [true, false, true, false] {
            let event = LinkEvent::<()>::Mute(state);
            assert!(mute.event(&event, &mut trace).unwrap());
            assert!(!mute.event(&event, &mut trace).unwrap());
            assert_eq!(
                mute.project(UiState::Listening),
                if state { UiState::Muted } else { UiState::Listening }
            );
        }
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        for (n, row) in rows.iter().enumerate() {
            assert_eq!(row["window_id"], format!("mute-{}", n + 1));
            assert_eq!(row["received_frames"], 0);
            assert_eq!(row["forwarded_frames"], 0);
        }
    }
    #[test]
    fn ordered_drop_counts_are_counted_once_only_inside_the_mute_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        let mut mute = Mute::default();
        for event in [
            LinkEvent::<()>::MicrophoneDropped(7),
            LinkEvent::Mute(true),
            LinkEvent::MicrophoneDropped(3),
            LinkEvent::MicrophoneDropped(2),
            LinkEvent::Mute(false),
            LinkEvent::MicrophoneDropped(9),
        ] {
            mute.event(&event, &mut trace).unwrap();
        }
        let row: serde_json::Value = serde_json::from_str(std::fs::read_to_string(path).unwrap().trim()).unwrap();
        assert_eq!(row["received_frames"], 5);
        assert_eq!(row["forwarded_frames"], 0);
    }
    #[test]
    fn send_accounting_still_exposes_a_broken_guard() {
        let mut mute = Mute {
            muted: true,
            ..Mute::default()
        };
        mute.sent();
        assert_eq!(mute.forwarded, 1);
    }
}
