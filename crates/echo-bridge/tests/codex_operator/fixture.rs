use super::*;
use echo_codex::codex_supervisor::{Supervisor, SupervisorConfig};

pub(super) struct Fixture {
    pub directory: tempfile::TempDir,
    pub home: PathBuf,
    pub binary: PathBuf,
    pub screen: std::net::SocketAddr,
    lifetimes: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<tokio::net::TcpStream>>,
    lifetime_task: tokio::task::JoinHandle<()>,
    observed: std::sync::atomic::AtomicUsize,
}

impl Fixture {
    pub fn new(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let binary = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(binary.status.success());
        let binary = PathBuf::from(String::from_utf8(binary.stdout).unwrap().trim());
        let home = directory.path().join("home");
        let owner = Supervisor::new(SupervisorConfig::new(binary.clone(), home.clone())).unwrap();
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
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let screen = listener.local_addr().unwrap();
        drop(listener);
        let (lifetimes, lifetime_task) = watch_children(tokio::net::TcpListener::from_std(lifetimes).unwrap());
        let fixture = Self {
            directory,
            home,
            binary,
            screen,
            lifetimes: tokio::sync::Mutex::new(lifetimes),
            lifetime_task,
            observed: std::sync::atomic::AtomicUsize::new(0),
        };
        fixture.configure(mode);
        fixture
    }

    fn configure(&self, mode: &str) {
        let path = self.directory.path();
        bridge_wire_auth::config(path, mode, "");
        std::fs::write(path.join("screen-token"), "synthetic-screen-pair").unwrap();
        let text = std::fs::read_to_string(path.join("bridge.toml"))
            .unwrap()
            .replace(
                "credentials/screen-pairing.txt",
                &path.join("screen-token").display().to_string().replace('\\', "/"),
            )
            .replace("127.0.0.1:0", &self.screen.to_string());
        std::fs::write(
            path.join("bridge.toml"),
            format!(
                "{text}\n[codex]\nhome = '{}'\nbinary = '{}'\n",
                self.home.display(),
                self.binary.display()
            ),
        )
        .unwrap();
    }

    pub fn owner(&self) -> Supervisor {
        Supervisor::new(SupervisorConfig::new(self.binary.clone(), self.home.clone())).unwrap()
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
