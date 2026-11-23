use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::IpAddr,
    path::Path,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

use echo_boot::{
    ChildStatus, ClockStatus, Digest, Family, HostOps, Identity, Log, Result, Role, Selection, Slot, Stop,
};

use super::super::{device_io::DeviceIo, jobs::Jobs};
use super::filter_fixture::{FilterFixture, golden};
use super::fixture_result;

pub(super) struct Fake<'a> {
    pub state: &'a Path,
    pub time: u64,
    pub end: u64,
    pub ready_current: bool,
    pub current: String,
    pub previous: Option<String>,
    pub deny: Option<Role>,
    pub exits: Vec<(u64, Role)>,
    pub parked: Option<(u64, u64)>,
    pub samples: Vec<u64>,
    pub jump: Option<(u64, u64)>,
    pub starts: Vec<(u64, Role, Slot, Identity)>,
    pub stops: Vec<(u64, Role, Stop, String)>,
    pub acks: Vec<(u64, bool)>,
    pub tries: Vec<(u64, u8)>,
    pub logs: Vec<String>,
    pub firewall: Option<FilterFixture>,
    pub services: Option<DeviceIo>,
    pub service_timeline: Option<FilterFixture>,
    pub clock_delay: Option<u64>,
    pub clock_starts: Vec<u64>,
    pub clock_stops: Vec<u64>,
    pub synchronize: Option<Box<dyn FnMut(u64) + 'a>>,
    jobs: Jobs,
    active_clock: Option<u64>,
    children: [Option<FakeChild>; 2],
}

struct FakeChild {
    child: Child,
    ready_at: Option<Duration>,
    exit: Option<String>,
}

impl<'a> Fake<'a> {
    pub(super) fn new(state: &'a Path, end: u64) -> Self {
        Self {
            state,
            end,
            time: 0,
            ready_current: true,
            current: "a".repeat(64),
            previous: Some("b".repeat(64)),
            deny: None,
            exits: vec![],
            parked: None,
            samples: vec![],
            jump: None,
            starts: vec![],
            stops: vec![],
            acks: vec![],
            tries: vec![],
            logs: vec![],
            firewall: None,
            services: None,
            service_timeline: None,
            clock_delay: Some(100),
            clock_starts: vec![],
            clock_stops: vec![],
            synchronize: None,
            jobs: Jobs::default(),
            active_clock: None,
            children: [None, None],
        }
    }

    pub(super) fn journal(&mut self) -> Selection {
        fixture_result(
            self.load_selection()
                .and_then(|state| state.ok_or("missing fake selection journal")),
        )
    }

    pub(super) fn times(&self, role: Role) -> Vec<u64> {
        self.starts
            .iter()
            .filter(|start| start.1 == role)
            .map(|start| start.0)
            .collect()
    }

    fn child(&mut self, role: Role) -> &mut Option<FakeChild> {
        &mut self.children[usize::from(role == Role::Core)]
    }
}

impl FakeChild {
    fn spawn(state: &Path, line: &str, now: Duration) -> Result<Self> {
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$1\"; IFS= read -r status; exit \"${status:-0}\"",
                "fake-role",
                line,
            ])
            .current_dir(state.parent().ok_or("missing fake installation directory")?)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| "spawn fake role")?;
        let mut fixture = Self {
            child,
            ready_at: None,
            exit: None,
        };
        let mut line = String::new();
        let stdout = fixture.child.stdout.take().ok_or("missing fake role output")?;
        BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(|_| "read fake role output")?;
        let receipt = OnceLock::new();
        super::super::receipt::drain(line.as_bytes(), &receipt).map_err(|_| "read fake role receipt")?;
        fixture.ready_at = receipt.get().map(|_| now);
        Ok(fixture)
    }

    fn finish(&mut self, code: u8) -> Result<String> {
        if let Some(status) = &self.exit {
            return Ok(status.clone());
        }
        let mut input = self.child.stdin.take().ok_or("missing fake role input")?;
        writeln!(input, "{code}").map_err(|_| "signal fake role exit")?;
        let status = self.child.wait().map_err(|_| "wait for fake role")?.to_string();
        self.exit = Some(status.clone());
        Ok(status)
    }
}

impl Drop for FakeChild {
    fn drop(&mut self) {
        if self.exit.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl HostOps for Fake<'_> {
    fn now(&self) -> Duration {
        Duration::from_millis(self.time)
    }

    fn sleep(&mut self, duration: Duration) {
        let millis = fixture_result(u64::try_from(duration.as_millis()));
        let mut next = self.time + millis;
        if let Some((from, to)) = self.jump
            && self.time == from
        {
            next = to;
        }
        for &sample in &self.samples {
            if sample > self.time {
                next = next.min(sample);
            }
        }
        if let Some(firewall) = &self.firewall {
            firewall.at(self.time, next);
        }
        if let Some(timeline) = &self.service_timeline {
            timeline.at(self.time, next);
        }
        if let Some(sync) = self.synchronize.as_mut() {
            sync(next);
        }
        self.time = next;
    }

    fn shutdown(&self) -> bool {
        self.time >= self.end
    }

    fn stop_marker(&mut self) -> Result<bool> {
        Ok(self
            .parked
            .is_some_and(|(start, end)| (start..end).contains(&self.time)))
    }

    fn acknowledge(&mut self, finished: bool) -> Result<()> {
        if finished {
            assert!(self.children.iter().all(Option::is_none));
        }
        self.acks.push((self.time, finished));
        Ok(())
    }

    fn candidate(&mut self, slot: Slot) -> Result<Digest> {
        let hex = match slot {
            Slot::Current => self.current.clone(),
            Slot::Previous => self.previous.clone().ok_or("open selected binary")?,
        };
        Digest::parse(hex)
    }

    fn load_selection(&mut self) -> Result<Option<Selection>> {
        match fs::read(self.state) {
            Ok(bytes) => Selection::decode(&bytes).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("read fake selection journal"),
        }
    }

    fn save_selection(&mut self, selection: &Selection) -> Result<()> {
        fs::write(self.state, selection.encode()?).map_err(|_| "write fake selection journal")?;
        let tries = selection.failed_tries();
        if self.tries.last().is_none_or(|last| last.1 != tries) {
            self.tries.push((self.time, tries));
        }
        Ok(())
    }

    fn spawn(&mut self, role: Role, slot: Slot, identity: Identity) -> Result<()> {
        if self.deny == Some(role) {
            return Err("denied role setup");
        }
        let ready = role == Role::Core && (self.ready_current || slot == Slot::Previous);
        let line = if ready {
            r#"{"event":"bridge.ready"}"#
        } else {
            r#"{"event":"unrelated"}"#
        };
        let child = FakeChild::spawn(self.state, line, self.now())?;
        assert!(self.child(role).replace(child).is_none());
        self.starts.push((self.time, role, slot, identity));
        Ok(())
    }

    fn observe(&mut self, role: Role) -> Result<ChildStatus> {
        let should_exit = self.exits.contains(&(self.time, role));
        let child = self.child(role).as_mut().ok_or("missing fake role")?;
        let ready_at = child.ready_at;
        if should_exit {
            child.finish(7)?;
            Ok(ChildStatus::Exited { ready_at })
        } else {
            Ok(ChildStatus::Running { ready_at })
        }
    }

    fn stop(&mut self, role: Role, why: Stop) -> Result<String> {
        let mut child = self.child(role).take().ok_or("missing fake role")?;
        let status = child.finish(0)?;
        self.stops.push((self.time, role, why, status.clone()));
        Ok(status)
    }

    fn log(&mut self, log: Log<'_>) {
        let line = match log {
            Log::Start(role, slot) => format!("start {} {}", role.name(), slot.name()),
            Log::Exit(role, why, status) => format!("exit {} {why:?} {status}", role.name()),
            Log::Fallback { current, previous } => format!("fallback {} {}", current.as_str(), previous.as_str()),
            Log::FirewallReady => "firewall ready".into(),
            Log::FirewallUnhealthy(stage) => format!("firewall unhealthy: {stage}"),
            Log::DeviceService(stage) => stage.into(),
            Log::Network(online) => format!("network {online}"),
        };
        self.logs.push(line);
    }

    fn resolvers(&mut self) -> Result<Vec<IpAddr>> {
        match &self.firewall {
            Some(firewall) => firewall.io.resolvers(&firewall.runtime),
            None => Ok(vec![
                IpAddr::from([192, 0, 2, 53]),
                IpAddr::from([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x53]),
            ]),
        }
    }

    fn filter(&mut self, family: Family) -> Result<Option<String>> {
        match &self.firewall {
            Some(firewall) => firewall.io.filter(&firewall.runtime, family),
            None => Ok(Some(golden(family).into())),
        }
    }

    fn restore_filter(&mut self, family: Family, transaction: &str) -> Result<()> {
        let firewall = self.firewall.as_ref().ok_or("missing fake filter commands")?;
        firewall.io.restore(&firewall.runtime, family, transaction)
    }

    fn ipv6_disabled(&mut self, enforce: bool) -> Result<bool> {
        self.firewall
            .as_ref()
            .ok_or("missing fake filter commands")?
            .io
            .ipv6_disabled(enforce)
    }

    fn usb_adb_only(&mut self, enforce: bool) -> Result<bool> {
        match &self.firewall {
            Some(firewall) => firewall.io.usb_adb_only(&firewall.runtime, enforce),
            None => Ok(true),
        }
    }

    fn prepare_device(&mut self) -> Result<()> {
        if let Some(device) = &self.services {
            device.prepare(|stage| self.logs.push(stage.into()));
        }
        Ok(())
    }

    fn poll_device(&mut self) -> Result<()> {
        Ok(())
    }

    fn stop_device(&mut self) -> Result<()> {
        Ok(())
    }

    fn network(&mut self) -> Result<Vec<IpAddr>> {
        match &self.services {
            Some(device) => device.commands.network(&device.runtime),
            None => self.resolvers(),
        }
    }

    fn start_clock(&mut self) -> Result<()> {
        if self.services.is_some() {
            let script = if self.clock_delay.is_some() {
                "printf 'clock: offset_ms=2000 hosts=3 action=set\n'"
            } else {
                "kill -STOP $$"
            };
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            self.jobs.start_clock(command)?;
            self.active_clock = Some(self.time);
            self.clock_starts.push(self.time);
        }
        Ok(())
    }

    fn poll_clock(&mut self) -> Result<ClockStatus> {
        match (self.active_clock, self.clock_delay) {
            (None, _) => Ok(ClockStatus::Unchanged),
            (Some(started), Some(delay)) if self.time >= started + delay => {
                self.jobs.await_clock_fixture();
                self.active_clock = None;
                self.jobs.poll_clock()
            }
            _ => Ok(ClockStatus::Running),
        }
    }

    fn stop_clock(&mut self) -> Result<()> {
        if self.active_clock.take().is_some() {
            self.jobs.stop_clock()?;
            self.clock_stops.push(self.time);
        }
        Ok(())
    }

    fn write_rtc(&mut self) -> Result<()> {
        let device = self.services.as_ref().ok_or("missing fake Android services")?;
        device.commands.write_rtc(&device.runtime)
    }
}
