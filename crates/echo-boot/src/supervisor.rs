use std::{net::IpAddr, time::Duration};

use crate::{ClockStatus, Digest, Family, Identity, Result, Role, Selection, Slot, firewall, services::Services};

const POLL: Duration = Duration::from_millis(100);
const STABLE: Duration = Duration::from_secs(60);
const RESTART: Duration = Duration::from_secs(3);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Debug, Eq, PartialEq)]
pub enum ChildStatus {
    Running {
        ready_at: Option<Duration>,
    },
    Exited {
        ready_at: Option<Duration>,
        /// Actual exit time in the host monotonic clock, when independently known.
        exited_at: Option<Duration>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    Exited,
    Timeout,
    Requested,
    SwitchSlot,
    Shutdown,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Log<'a> {
    Start(Role, Slot),
    Exit(Role, Stop, &'a str),
    Fallback { current: &'a Digest, previous: &'a Digest },
    FirewallReady,
    FirewallUnhealthy(&'a str),
    DeviceService(&'static str),
    Network(bool),
}

/// Owns the environmental effects and the live child handles for the boot roles.
pub trait HostOps {
    fn now(&self) -> Duration;
    fn sleep(&mut self, duration: Duration);
    fn shutdown(&self) -> bool;
    /// # Errors
    /// The stop marker could not be inspected safely.
    fn stop_marker(&mut self) -> Result<bool>;
    /// # Errors
    /// Publishing or clearing the acknowledgement failed.
    fn acknowledge(&mut self, finished: bool) -> Result<()>;
    /// # Errors
    /// The candidate could not be validated or hashed.
    fn candidate(&mut self, slot: Slot) -> Result<Digest>;
    /// # Errors
    /// The persisted journal could not be read or decoded.
    fn load_selection(&mut self) -> Result<Option<Selection>>;
    /// # Errors
    /// The selection journal could not be saved durably.
    fn save_selection(&mut self, selection: &Selection) -> Result<()>;
    /// # Errors
    /// The launcher failed to validate, apply or start the role.
    fn spawn(&mut self, role: Role, slot: Slot, identity: Identity) -> Result<()>;
    /// # Errors
    /// The role's process state could not be inspected.
    fn observe(&mut self, role: Role) -> Result<ChildStatus>;
    /// Finishes cleanup and reaps the process before returning its status.
    ///
    /// # Errors
    /// The role or its descendants could not be stopped and reaped.
    fn stop(&mut self, role: Role, why: Stop) -> Result<String>;
    fn log(&mut self, entry: Log<'_>);
    /// # Errors
    /// The resolver properties could not be read.
    fn resolvers(&mut self) -> Result<Vec<IpAddr>>;
    /// None denotes a missing tool.
    ///
    /// # Errors
    /// An available tool failed to list the filter.
    fn filter(&mut self, family: Family) -> Result<Option<String>>;
    /// # Errors
    /// The filter restore tool could not apply the transaction.
    fn restore_filter(&mut self, family: Family, transaction: &str) -> Result<()>;
    /// # Errors
    /// IPv6 settings could not be read or applied.
    fn ipv6_disabled(&mut self, enforce: bool) -> Result<bool>;
    /// # Errors
    /// The ADB properties or listeners could not be read or corrected.
    fn usb_adb_only(&mut self, enforce: bool) -> Result<bool>;
    /// # Errors
    /// The Android startup service could not be started.
    fn prepare_device(&mut self) -> Result<()>;
    /// # Errors
    /// The startup service failed or could not be observed.
    fn poll_device(&mut self) -> Result<()>;
    /// # Errors
    /// The startup service group could not be stopped and reaped.
    fn stop_device(&mut self) -> Result<()>;
    /// # Errors
    /// The Wi-Fi link or resolver state could not be read.
    fn network(&mut self) -> Result<Vec<IpAddr>>;
    /// # Errors
    /// The clock sync worker could not be started.
    fn start_clock(&mut self) -> Result<()>;
    /// # Errors
    /// The clock worker failed or returned an invalid receipt.
    fn poll_clock(&mut self) -> Result<ClockStatus>;
    /// # Errors
    /// The clock worker could not be stopped and reaped.
    fn stop_clock(&mut self) -> Result<()>;
    /// # Errors
    /// The Android RTC command failed.
    fn write_rtc(&mut self) -> Result<()>;
}

/// Runs one supervisor, including reconciliation of a previously interrupted attempt.
///
/// # Errors
/// Returns a stage error from the host on a failed layout, launch or cleanup operation.
pub fn supervise(host: &mut impl HostOps) -> Result<()> {
    let mut supervisor = Supervisor::default();
    let outcome = supervisor.run(host);
    let cleanup = supervisor.stop_all(host, Stop::Shutdown);
    let services = supervisor.services.stop(host);
    cleanup.and(services).and(outcome)
}

#[derive(Default)]
struct Supervisor {
    device: Lifecycle,
    core: Lifecycle,
    selection: Option<Selection>,
    parked: bool,
    firewall_retry_at: Duration,
    firewall_error: Option<&'static str>,
    services: Services,
}

#[derive(Default)]
struct Lifecycle {
    started: Option<Duration>,
    restart_at: Duration,
    backoff: Option<Duration>,
    stable: bool,
}

impl Lifecycle {
    fn start(&mut self, now: Duration) {
        self.started = Some(now);
        self.stable = false;
    }

    fn retry(&mut self, now: Duration, elapsed: Duration) {
        let delay = if elapsed < STABLE {
            self.backoff.map_or(RESTART, |last| (last * 2).min(MAX_BACKOFF))
        } else {
            RESTART
        };
        self.backoff = (elapsed < STABLE).then_some(delay);
        self.restart_at = now + delay;
    }
}

impl Supervisor {
    fn run(&mut self, host: &mut impl HostOps) -> Result<()> {
        while !host.shutdown() {
            if host.stop_marker()? {
                self.park(host)?;
            } else {
                if self.parked || self.selection.is_none() {
                    if !self.firewall_ready(host) {
                        host.sleep(POLL);
                        continue;
                    }
                    self.resume(host)?;
                }
                self.observe(host, Role::Core)?;
                self.observe(host, Role::Device)?;
                self.start_due(host, Role::Device)?;
                if self.services.tick(host) {
                    self.start_due(host, Role::Core)?;
                }
            }
            host.sleep(POLL);
        }
        Ok(())
    }

    fn firewall_ready(&mut self, host: &mut impl HostOps) -> bool {
        if host.now() < self.firewall_retry_at {
            return false;
        }
        match firewall::check_firewall(host) {
            Ok(()) => {
                self.firewall_error = None;
                true
            }
            Err(stage) => {
                if self.firewall_error != Some(stage) {
                    host.log(Log::FirewallUnhealthy(stage));
                }
                self.firewall_error = Some(stage);
                self.firewall_retry_at = host.now() + firewall::WATCH;
                false
            }
        }
    }

    fn park(&mut self, host: &mut impl HostOps) -> Result<()> {
        if self.parked {
            return Ok(());
        }
        if let Some(ref mut selection) = self.selection {
            selection.cancel();
            host.save_selection(selection)?;
        } else if let Some(mut selection) = host.load_selection()? {
            selection.cancel();
            host.save_selection(&selection)?;
        }
        self.stop_all(host, Stop::Requested)?;
        self.services.stop(host)?;
        host.acknowledge(true)?;
        self.parked = true;
        Ok(())
    }

    fn resume(&mut self, host: &mut impl HostOps) -> Result<()> {
        host.acknowledge(false)?;
        let current = host.candidate(Slot::Current)?;
        let stored = host.load_selection()?;
        let mut selection = stored
            .filter(|state| *state.current() == current)
            .unwrap_or_else(|| Selection::new(current));
        selection.fail();
        host.save_selection(&selection)?;
        if selection.slot() == Slot::Previous {
            host.candidate(Slot::Previous)?;
        }
        self.selection = Some(selection);
        self.switch_if_needed(host)?;
        self.device = Lifecycle::default();
        self.core = Lifecycle::default();
        self.parked = false;
        Ok(())
    }

    fn selection(&mut self) -> Result<&mut Selection> {
        self.selection.as_mut().ok_or("selection unavailable")
    }

    fn life(&mut self, role: Role) -> &mut Lifecycle {
        match role {
            Role::Device => &mut self.device,
            Role::Core => &mut self.core,
        }
    }

    fn start_due(&mut self, host: &mut impl HostOps, role: Role) -> Result<()> {
        let life = self.life(role);
        if life.started.is_some() || host.now() < life.restart_at {
            return Ok(());
        }
        let selection = self.selection()?;
        if role == Role::Core {
            selection.attempt();
            host.save_selection(selection)?;
        }
        let slot = selection.slot();
        host.log(Log::Start(role, slot));
        if let Err(error) = host.spawn(role, slot, role.identity()) {
            host.log(Log::Exit(role, Stop::Shutdown, error));
            if role == Role::Core {
                let selection = self.selection()?;
                selection.fail();
                host.save_selection(selection)?;
            }
            return Err(error);
        }
        self.life(role).start(host.now());
        Ok(())
    }

    fn observe(&mut self, host: &mut impl HostOps, role: Role) -> Result<()> {
        let Some(started) = self.life(role).started else {
            return Ok(());
        };
        let now = host.now();
        let elapsed = now.saturating_sub(started);
        let status = host.observe(role)?;
        let (ready_at, alive_until) = match status {
            ChildStatus::Running { ready_at } => (ready_at, Some(now)),
            ChildStatus::Exited { ready_at, exited_at } => (ready_at, exited_at),
        };
        let exited = matches!(status, ChildStatus::Exited { .. });
        if role == Role::Core && elapsed >= STABLE && !self.core.stable {
            let deadline = started + STABLE;
            if ready_at.is_some_and(|at| at <= deadline) && alive_until.is_some_and(|at| at >= deadline) {
                let selection = self.selection()?;
                selection.stabilize();
                host.save_selection(selection)?;
                self.core.stable = true;
            } else if !exited {
                return self.restart_after_stop(host, role, Stop::Timeout, now, elapsed);
            }
        }
        if exited {
            self.restart_after_stop(host, role, Stop::Exited, now, elapsed)?;
        }
        Ok(())
    }

    fn restart_after_stop(
        &mut self,
        host: &mut impl HostOps,
        role: Role,
        why: Stop,
        now: Duration,
        elapsed: Duration,
    ) -> Result<()> {
        self.finish(host, role, why)?;
        self.life(role).retry(now, elapsed);
        if role == Role::Core && !self.core.stable {
            let selection = self.selection()?;
            selection.fail();
            host.save_selection(selection)?;
            self.switch_if_needed(host)?;
        }
        Ok(())
    }

    fn switch_if_needed(&mut self, host: &mut impl HostOps) -> Result<()> {
        if !self.selection()?.needs_fallback() {
            return Ok(());
        }
        self.stop_all(host, Stop::SwitchSlot)?;
        let previous = host.candidate(Slot::Previous)?;
        let selection = self.selection()?;
        selection.fallback();
        host.save_selection(selection)?;
        host.log(Log::Fallback {
            current: selection.current(),
            previous: &previous,
        });
        self.device = Lifecycle {
            restart_at: self.core.restart_at,
            ..Lifecycle::default()
        };
        Ok(())
    }

    fn stop_all(&mut self, host: &mut impl HostOps, why: Stop) -> Result<()> {
        let [core, device] = [Role::Core, Role::Device].map(|role| self.finish(host, role, why));
        core.and(device)
    }

    fn finish(&mut self, host: &mut impl HostOps, role: Role, why: Stop) -> Result<()> {
        if self.life(role).started.is_some() {
            let status = host.stop(role, why)?;
            self.life(role).started = None;
            host.log(Log::Exit(role, why, &status));
        }
        Ok(())
    }
}
