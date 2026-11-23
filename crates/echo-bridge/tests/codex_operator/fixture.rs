use super::*;
use echo_codex::codex_supervisor::{HomeMode, Supervisor, SupervisorConfig};
use echo_config::LiveMode;
use std::net::{Ipv4Addr, SocketAddr};

pub(super) struct Fixture {
    pub directory: tempfile::TempDir,
    pub home: PathBuf,
    pub configured_home: PathBuf,
    pub binary: PathBuf,
    home_mode: HomeMode,
    lifetimes: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<tokio::net::TcpStream>>,
    lifetime_task: tokio::task::JoinHandle<()>,
    observed: std::sync::atomic::AtomicUsize,
}

impl Fixture {
    pub fn new(mode: &str) -> Self {
        let live_mode = if mode == "api_key" {
            LiveMode::ApiKey
        } else {
            LiveMode::Chatgpt
        };
        Self::with_live_mode(mode, live_mode)
    }

    pub fn with_live_mode(mode: &str, live_mode: LiveMode) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let binary = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(binary.status.success());
        let binary = PathBuf::from(String::from_utf8(binary.stdout).unwrap().trim());
        let configured_home = directory.path().join("home");
        let (home, home_mode, config_mode) = match live_mode {
            LiveMode::Chatgpt => (configured_home.clone(), HomeMode::Single, "chatgpt"),
            LiveMode::ApiKey => (directory.path().join("home-api-key"), HomeMode::ApiKey, "api_key"),
        };
        let mut config = SupervisorConfig::new(binary.clone(), configured_home.clone());
        config.home_mode = home_mode;
        let owner = Supervisor::new(config).unwrap();
        std::fs::write(home.join("work/app-server"), include_str!("peer.py")).unwrap();
        std::fs::write(home.join("mode"), mode).unwrap();
        let lifetimes = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::write(
            home.join("lifetime-port"),
            lifetimes.local_addr().unwrap().port().to_string(),
        )
        .unwrap();
        lifetimes.set_nonblocking(true).unwrap();
        drop(owner);
        let (lifetimes, lifetime_task) = watch_children(tokio::net::TcpListener::from_std(lifetimes).unwrap());
        let fixture = Self {
            directory,
            home,
            configured_home,
            binary,
            home_mode,
            lifetimes: tokio::sync::Mutex::new(lifetimes),
            lifetime_task,
            observed: std::sync::atomic::AtomicUsize::new(0),
        };
        fixture.configure(config_mode);
        fixture
    }

    fn configure(&self, mode: &str) {
        let path = self.directory.path();
        bridge_wire_auth::config(path, mode, "");
        std::fs::write(path.join("screen-token"), "synthetic-screen-pair").unwrap();
        let text = std::fs::read_to_string(path.join("bridge.toml")).unwrap().replace(
            "credentials/screen-pairing.txt",
            &path.join("screen-token").display().to_string().replace('\\', "/"),
        );
        std::fs::write(
            path.join("bridge.toml"),
            format!(
                "{text}\n[codex]\nhome = '{}'\nbinary = '{}'\n",
                self.configured_home.display(),
                self.binary.display()
            ),
        )
        .unwrap();
    }

    pub fn owner(&self) -> Supervisor {
        let mut config = SupervisorConfig::new(self.binary.clone(), self.configured_home.clone());
        config.home_mode = self.home_mode;
        Supervisor::new(config).unwrap()
    }

    pub async fn child_lifetime(&self) -> tokio::net::TcpStream {
        self.lifetimes.lock().await.recv().await.unwrap()
    }

    pub async fn children_released(&self) {
        use std::sync::atomic::Ordering;
        let launches = std::fs::read_to_string(self.home.join("launches"))
            .unwrap()
            .lines()
            .count();
        for _ in self.observed.load(Ordering::SeqCst)..launches {
            Self::child_closed(self.child_lifetime().await).await;
            self.observed.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(self.observed.load(Ordering::SeqCst), launches);
    }

    pub async fn child_closed(mut stream: tokio::net::TcpStream) {
        // Process termination can reset held TCP connections; both EOF
        // and reset prove destruction of this child's connected socket.
        match stream.read(&mut [0]).await {
            Ok(count) => assert_eq!(count, 0),
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
        }
    }

    pub async fn control(&self) -> tokio::net::TcpStream {
        let port = tokio::time::timeout(OBSERVATION_TIMEOUT, async {
            loop {
                if let Ok(text) = std::fs::read_to_string(self.home.join("peer-port"))
                    && let Ok(port) = text.parse::<u16>()
                {
                    break port;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap()
    }
}

/// The trace is append-only across runtime generations and process images.
/// Count only complete `screen.listening` records in the caller's trace.
pub(super) async fn screen_address(path: &Path, generation: usize) -> SocketAddr {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        let mut polls = tokio::time::interval(POLL_INTERVAL);
        loop {
            polls.tick().await;
            if let Some(address) = traced_screen(path, generation) {
                break address;
            }
        }
    })
    .await
    .map_err(|_| format!("missing screen.listening for generation {generation} in {path:?}"))
    .unwrap()
}

fn traced_screen(path: &Path, generation: usize) -> Option<SocketAddr> {
    let bytes = std::fs::read(path)
        .map(Some)
        .or_else(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Ok(None),
            _ => Err(error),
        })
        .expect("read bridge screen trace")?;
    let end = bytes.iter().rposition(|&byte| byte == b'\n')?;
    let row = bytes[..end]
        .split(|&byte| byte == b'\n')
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .filter(|row| row["event"] == "screen.listening")
        .nth(generation)?;
    let address: SocketAddr = row["address"].as_str().unwrap().parse().unwrap();
    assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
    assert_ne!(address.port(), 0);
    Some(address)
}

/// Standalone sign-in publishes a prompt without a runtime trace. Resolve
/// only IPv4 listeners whose socket inodes are held by that bridge child.
pub(super) async fn signin_screen(process: &mut Process) -> SocketAddr {
    let pid = process.child.id().expect("sign-in child PID after prompt");
    let proc = PathBuf::from(format!("/proc/{pid}"));
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        let mut polls = tokio::time::interval(POLL_INTERVAL);
        loop {
            polls.tick().await;
            let addresses = child_listeners(&proc);
            let status = process.child.try_wait().unwrap();
            assert!(status.is_none(), "sign-in bridge exited after prompt: {status:?}");
            let addresses = addresses.map_err(|error| format!("reading sign-in bridge {pid} listeners: {error}"))?;
            match addresses.as_slice() {
                [] => (),
                [address] => break Ok(*address),
                addresses => {
                    break Err(format!(
                        "sign-in bridge {pid} has ambiguous loopback listeners: {addresses:?}"
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| format!("sign-in bridge {pid} did not expose an IPv4 loopback listener after its prompt"))
    .unwrap()
    .unwrap()
}

fn child_listeners(proc: &Path) -> std::io::Result<Vec<SocketAddr>> {
    let mut sockets = std::collections::HashSet::new();
    for fd in std::fs::read_dir(proc.join("fd"))? {
        let target = match std::fs::read_link(fd?.path()) {
            Ok(target) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if let Some(inode) = target.to_str().and_then(|link| link.strip_prefix("socket:[")) {
            sockets.insert(inode.strip_suffix(']').unwrap().to_owned());
        }
    }
    let tcp = std::fs::read_to_string(proc.join("net/tcp"))?;
    Ok(tcp
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields[3] != "0A" || !sockets.contains(fields[9]) {
                return None;
            }
            let (ip, port) = fields[1].split_once(':').unwrap();
            let ip = Ipv4Addr::from(u32::from_str_radix(ip, 16).unwrap().to_ne_bytes());
            let port = u16::from_str_radix(port, 16).unwrap();
            (ip == Ipv4Addr::LOCALHOST && port != 0).then_some((ip, port).into())
        })
        .collect())
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.lifetime_task.abort();
    }
}

fn watch_children(
    listener: tokio::net::TcpListener,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<tokio::net::TcpStream>,
    tokio::task::JoinHandle<()>,
) {
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert_eq!(stream.read_u8().await.unwrap(), b'O');
            // Admit the child only once its instance-owned connection is held
            // by this test. Process kill may discard unread bytes.
            stream.write_all(b"A").await.unwrap();
            if send.send(stream).is_err() {
                break;
            }
        }
    });
    (receive, task)
}

/// The bridge's `run-satellite` registration data. The pinned hassmic reads
/// it only from the frame's data segment, never inline in the header.
pub(super) async fn registration(satellite: &mut tokio::net::TcpStream) -> Value {
    let mut decoder = echo_contracts::wyoming::Decoder::new();
    loop {
        let byte = tokio::io::AsyncReadExt::read_u8(satellite).await.unwrap();
        if let Some(event) = decoder.feed(&[byte]).unwrap().pop() {
            assert_eq!(event.header.r#type, "run-satellite");
            assert_eq!(event.header.data, None);
            return event.data.unwrap();
        }
    }
}
