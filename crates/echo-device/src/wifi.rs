//! Wi-Fi mutation policy shared by the device adapter and its simulated radio.

/// The Wi-Fi link observed after the radio settles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// Associated network, if any.
    pub ssid: Option<String>,
    /// Whether the Wi-Fi link has a usable address and route.
    pub reachable: bool,
}

/// A provider-local transaction. The provider keeps saved credentials private.
pub trait Join {
    /// Snapshot the active and affected profiles. False means no mutation is safe.
    fn prepare(&mut self, ssid: &str, password: &str) -> bool;
    /// Apply the candidate and wait for a fresh association with its profile.
    fn associate(&mut self) -> Option<Status>;
    /// Reconcile the snapshot and verify the prior profile and link state.
    fn restore(&mut self) -> Option<Status>;
    /// Observe the actual link after a failed reconciliation.
    fn status(&mut self) -> Option<Status>;
}

/// Outcome of a serialized join.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Snapshot unavailable. No change was attempted.
    Unavailable,
    /// The target network was observed with reachability.
    Connected(Status),
    /// The previous state was restored and observed.
    Rejected(Status),
    /// The previous state could not be verified. Includes any actual status.
    RestoreFailed(Option<Status>),
}

/// Run one complete join even if the requesting client has disconnected.
pub fn join(radio: &mut impl Join, ssid: &str, password: &str) -> Outcome {
    if !radio.prepare(ssid, password) {
        return Outcome::Unavailable;
    }
    if let Some(status) = radio.associate()
        && status.reachable
        && status.ssid.as_deref() == Some(ssid)
    {
        return Outcome::Connected(status);
    }
    if let Some(status) = radio.restore() {
        return Outcome::Rejected(status);
    }
    Outcome::RestoreFailed(radio.status())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Radio {
        active: Status,
        profile: u32,
        snapshot: Option<(Status, u32)>,
        can_snapshot: bool,
        can_restore: bool,
        reaches_candidate: bool,
    }

    impl Radio {
        fn new() -> Self {
            Self {
                active: status("Fixture A", true),
                profile: 7,
                snapshot: None,
                can_snapshot: true,
                can_restore: true,
                reaches_candidate: false,
            }
        }
    }

    impl Join for Radio {
        fn prepare(&mut self, _: &str, _: &str) -> bool {
            if self.can_snapshot {
                self.snapshot = Some((self.active.clone(), self.profile));
            }
            self.can_snapshot
        }

        fn associate(&mut self) -> Option<Status> {
            self.profile = 8;
            self.active.reachable = self.reaches_candidate;
            self.status()
        }

        fn restore(&mut self) -> Option<Status> {
            if self.can_restore {
                (self.active, self.profile) = self.snapshot.take().unwrap();
                return self.status();
            }
            None
        }

        fn status(&mut self) -> Option<Status> {
            Some(self.active.clone())
        }
    }

    fn status(ssid: &str, reachable: bool) -> Status {
        Status {
            ssid: Some(ssid.into()),
            reachable,
        }
    }

    #[test]
    fn wifi_same_ssid_without_a_route_restores_the_exact_snapshot() {
        let mut radio = Radio::new();
        let expected = status("Fixture A", true);
        assert_eq!(join(&mut radio, "Fixture A", ""), Outcome::Rejected(expected.clone()));
        assert_eq!((radio.active, radio.profile), (expected, 7));
    }

    #[test]
    fn wifi_a_reachable_different_network_does_not_confirm_a_join() {
        let mut radio = Radio {
            reaches_candidate: true,
            ..Radio::new()
        };
        assert_eq!(
            join(&mut radio, "Fixture B", ""),
            Outcome::Rejected(status("Fixture A", true))
        );
        assert_eq!(radio.profile, 7);
    }

    #[test]
    fn wifi_snapshot_refusal_never_writes_and_failed_restore_reports_observed_state() {
        let mut radio = Radio {
            can_snapshot: false,
            ..Radio::new()
        };
        assert_eq!(join(&mut radio, "Fixture A", ""), Outcome::Unavailable);
        assert_eq!(radio.profile, 7);
        radio.can_snapshot = true;
        radio.can_restore = false;
        assert_eq!(
            join(&mut radio, "Fixture A", ""),
            Outcome::RestoreFailed(Some(status("Fixture A", false)))
        );
        assert_eq!(radio.profile, 8);
    }
}
