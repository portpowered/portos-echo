use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

use echo_boot::{ChildStatus, Digest, HostOps, Identity, Log, Result, Role, Selection, Slot, Stop};

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
    pub starts: Vec<(u64, Role, Slot, Identity)>,
    pub stops: Vec<(u64, Role, Stop, String)>,
    pub acks: Vec<(u64, bool)>,
    pub tries: Vec<(u64, u8)>,
    pub logs: Vec<String>,
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
            starts: vec![],
            stops: vec![],
            acks: vec![],
            tries: vec![],
            logs: vec![],
            children: [None, None],
        }
    }

    pub(super) fn journal(&mut self) -> Selection {
        self.load_selection().unwrap().unwrap()
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
    fn spawn(state: &Path, line: &str, now: Duration) -> Self {
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$1\"; IFS= read -r status; exit \"${status:-0}\"",
                "fake-role",
                line,
            ])
            .current_dir(state.parent().unwrap())
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let receipt = OnceLock::new();
        super::super::receipt::drain(line.as_bytes(), &receipt).unwrap();
        Self {
            child,
            ready_at: receipt.get().map(|_| now),
            exit: None,
        }
    }

    fn finish(&mut self, code: u8) -> String {
        self.exit
            .get_or_insert_with(|| {
                writeln!(self.child.stdin.take().unwrap(), "{code}").unwrap();
                self.child.wait().unwrap().to_string()
            })
            .clone()
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
        let mut next = self.time + u64::try_from(duration.as_millis()).unwrap();
        for &sample in &self.samples {
            if sample > self.time {
                next = next.min(sample);
            }
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
        fs::write(self.state, selection.encode()?).unwrap();
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
        let child = FakeChild::spawn(self.state, line, self.now());
        assert!(self.child(role).replace(child).is_none());
        self.starts.push((self.time, role, slot, identity));
        Ok(())
    }

    fn observe(&mut self, role: Role) -> Result<ChildStatus> {
        let should_exit = self.exits.contains(&(self.time, role));
        let child = self.child(role).as_mut().unwrap();
        if should_exit {
            child.finish(7);
            Ok(ChildStatus::Exited)
        } else {
            Ok(ChildStatus::Running {
                ready_at: child.ready_at,
            })
        }
    }

    fn stop(&mut self, role: Role, why: Stop) -> Result<String> {
        let mut child = self.child(role).take().unwrap();
        let status = child.finish(0);
        self.stops.push((self.time, role, why, status.clone()));
        Ok(status)
    }

    fn log(&mut self, log: Log<'_>) {
        let line = match log {
            Log::Start(role, slot) => format!("start {} {}", role.name(), slot.name()),
            Log::Exit(role, why, status) => format!("exit {} {why:?} {status}", role.name()),
            Log::Fallback { current, previous } => format!("fallback {} {}", current.as_str(), previous.as_str()),
        };
        self.logs.push(line);
    }
}
