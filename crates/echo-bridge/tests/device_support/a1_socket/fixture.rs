use super::*;
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::PermissionsExt,
};
use tokio::process::{Child, ChildStderr};

const BRIDGE: &str = env!("CARGO_BIN_EXE_echo-bridge");
pub(super) const NAME: &str = "Lab-Sat";
pub(super) const PORT: &str = "16700";
pub(super) const TCP: &str = "127.0.0.1:16700";
pub(super) const SAMPLE: [u8; 4] = [0x0a, 0x02, 0xf6, 0xfd];

pub(super) async fn isolated(case: &str, run: impl Future<Output = ()>) {
    let key = "ECHO_T3_A1_SOCKET_PARENT_NETNS";
    let namespace = fs::read_link("/proc/self/ns/net").unwrap();
    if let Some(parent) = std::env::var_os(key) {
        assert_ne!(namespace.as_os_str(), parent);
        run.await;
        return;
    }
    let status = Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "--", "/bin/sh", "-c"])
        .arg("ip link set lo up && exec \"$@\"")
        .arg("sh")
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", &format!("a1_socket::{case}"), "--ignored", "--nocapture"])
        .env(key, namespace)
        .kill_on_drop(true)
        .status()
        .await
        .unwrap();
    assert!(status.success(), "isolated {case}: {status}");
}

pub(super) struct Fixture {
    root: tempfile::TempDir,
    pub(super) socket: PathBuf,
    pub(super) trace: PathBuf,
    keys: File,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        fs::copy(BRIDGE, dir.join("hassmic")).unwrap();
        fs::write(dir.join("cap"), SAMPLE.repeat(400)).unwrap();
        fs::write(dir.join("addrs"), "127.0.0.1\n").unwrap();
        fs::create_dir(dir.join("state")).unwrap();
        fs::write(dir.join("state/config"), "even_volume=off\n").unwrap();
        fs::create_dir(dir.join("mixer")).unwrap();
        fs::write(dir.join("mixer/MainVolume"), "40\n").unwrap();
        let run = dir.join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o750)).unwrap();
        let keys = dir.join("keys");
        rustix::fs::mkfifoat(rustix::fs::CWD, &keys, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR).unwrap();
        let keys = OpenOptions::new().read(true).write(true).open(keys).unwrap();
        Self {
            socket: run.join("wyoming.sock"),
            trace: dir.join("bridge.jsonl"),
            root,
            keys,
        }
    }

    pub(super) fn device(&self) -> Command {
        let dir = self.root.path();
        let mut command = Command::new(dir.join("hassmic"));
        command
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .current_dir(dir)
            .env_clear()
            .env("HASSMIC_CAP", dir.join("cap"))
            .env("HASSMIC_PLAY", dir.join("play"))
            .env("HASSMIC_STATE", dir.join("state"))
            .env("HASSMIC_FAKE_MIXER", dir.join("mixer"))
            .env("HASSMIC_FAKE_LED", dir.join("led"))
            .env("HASSMIC_FAKE_ADDRS", dir.join("addrs"))
            .env("HASSMIC_MDNS_FILE", dir.join("no-mdns"))
            .args([
                "-P", "wyoming", "-D", "-E", "-p", PORT, "-b", "keys", "-n", NAME, "-a", "0",
            ]);
        command
    }

    pub(super) async fn start(&self) -> Satellite {
        let mut child = self
            .device()
            .arg("--wyoming-socket")
            .arg(&self.socket)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let logs = BufReader::new(child.stderr.take().unwrap()).lines();
        let mut satellite = Satellite { child, logs };
        satellite.wait_for_log("wyoming on Unix socket").await;
        satellite
    }

    pub(super) fn bridge(&self, live: &str) -> Process {
        let mut command = super::super::bridge(PORT, live, &self.trace);
        Process::spawn(command.env("ECHO_LAB_WYOMING_SOCKET", &self.socket))
    }

    pub(super) fn toggle_mic(&mut self) {
        // Native Linux input_event followed by EV_KEY/KEY_MUTE key-up.
        let mut event = vec![0; 2 * size_of::<isize>()];
        event.extend(1_u16.to_ne_bytes());
        event.extend(113_u16.to_ne_bytes());
        event.extend(0_i32.to_ne_bytes());
        self.keys.write_all(&event).unwrap();
    }
}

pub(super) struct Satellite {
    pub(super) child: Child,
    logs: Lines<BufReader<ChildStderr>>,
}

impl Satellite {
    pub(super) async fn wait_for_log(&mut self, marker: &str) {
        let mut observed = Vec::new();
        timeout(LIMIT, async {
            while let Some(line) = self.logs.next_line().await.unwrap() {
                if line.contains(marker) {
                    return;
                }
                observed.push(line);
            }
            panic!("satellite exited before {marker}: {observed:?}");
        })
        .await
        .unwrap_or_else(|_| panic!("satellite deadline for {marker}: {observed:?}"));
    }

    pub(super) async fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([signal, &self.child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap();
        assert!(status.success());
    }
}

pub(super) fn listener_counts(socket: &Path) -> (usize, usize) {
    let unix = fs::read_to_string("/proc/self/net/unix")
        .unwrap()
        .lines()
        .skip(1)
        .filter(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.get(3) == Some(&"00010000") && fields.get(7).copied() == socket.to_str()
        })
        .count();
    let tcp = ["/proc/self/net/tcp", "/proc/self/net/tcp6"]
        .iter()
        .map(|file| {
            fs::read_to_string(file)
                .unwrap()
                .lines()
                .skip(1)
                .filter(|line| {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    fields[1].ends_with(":413C") && fields[3] == "0A" // 16700, LISTEN
                })
                .count()
        })
        .sum();
    (unix, tcp)
}
