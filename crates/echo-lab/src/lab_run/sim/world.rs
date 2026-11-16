use std::path::{Path, PathBuf};
use std::time::Duration;

use echo_live::fake::{FakeLive, FakeResponse, FakeScript};
use echo_spoof_mcp::{Options as SpoofOptions, Spoof};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::process::Command;

use super::{
    process::{self, Process},
    trace::Trace,
};

#[derive(Default)]
pub struct World {
    pub host: Option<Process>,
    pub bridge: Option<Process>,
    spoof: Option<Spoof>,
    live: Option<FakeLive>,
    pub trace: Trace,
    trace_path: PathBuf,
    pub ui_path: PathBuf,
    offset: u64,
    pub run: Option<String>,
    pub endpoints: Vec<std::net::SocketAddr>,
}

impl World {
    pub async fn start(&mut self, python: &str, bridge: &Path, root: &Path) -> Result<(), String> {
        self.start_with(
            python,
            bridge,
            root,
            FakeLive::bind(FakeScript {
                events: vec![],
                response: FakeResponse::Answer,
            }),
        )
        .await
    }

    pub(super) async fn start_with(
        &mut self,
        python: &str,
        bridge: &Path,
        root: &Path,
        live: impl std::future::Future<Output = Result<FakeLive, echo_live::call::CallError>>,
    ) -> Result<(), String> {
        self.start_owned(&mut process::host(python), bridge, root, live, process::connect)
            .await
    }

    async fn start_owned<F: std::future::Future<Output = bool>>(
        &mut self,
        host_command: &mut Command,
        bridge: &Path,
        root: &Path,
        live: impl std::future::Future<Output = Result<FakeLive, echo_live::call::CallError>>,
        probe: impl FnMut(std::net::SocketAddr) -> F,
    ) -> Result<(), String> {
        self.trace_path = root.join("bridge.private.jsonl");
        self.ui_path = root.join("ui.private.json");
        let deadline = process::readiness_deadline();
        self.host = Some(Process::start(host_command)?);
        let host = self.host.as_mut().ok_or("launch: missing host")?;
        let address = process::admit_host(&mut self.endpoints, host.expect("host.ready"), deadline, probe).await?;
        self.spoof = Some(
            Spoof::start(SpoofOptions::default())
                .await
                .map_err(|_| "launch: spoof startup failed")?,
        );
        process::admit(
            &mut self.endpoints,
            Self::endpoint(self.spoof.as_ref().ok_or("launch: missing spoof")?.url())?,
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        self.live = Some(live.await.map_err(|_| "launch: fake Live startup failed")?);
        process::admit(
            &mut self.endpoints,
            Self::endpoint(&self.live.as_ref().ok_or("launch: missing fake Live")?.base_url())?,
            process::readiness_deadline(),
            process::connect,
        )
        .await?;
        let mut command = Command::new(bridge);
        command
            .env("ECHO_LAB_WYOMING_ADDR", address.to_string())
            .env(
                "ECHO_LAB_MCP_URL",
                self.spoof.as_ref().ok_or("launch: missing spoof")?.url(),
            )
            .env(
                "ECHO_LAB_LIVE_URL",
                self.live.as_ref().ok_or("launch: missing fake Live")?.base_url(),
            )
            .env("ECHO_LAB_TRACE_PATH", &self.trace_path)
            .env("ECHO_LAB_UI_STATE_PATH", &self.ui_path);
        self.bridge = Some(Process::start(&mut command)?);
        self.bridge
            .as_mut()
            .ok_or("launch: missing bridge")?
            .expect("bridge.ready")
            .await?;
        self.control(&json!({"action":"registered"})).await
    }

    fn endpoint(url: &str) -> Result<std::net::SocketAddr, String> {
        let address: std::net::SocketAddr = url
            .strip_prefix("http://")
            .and_then(|s| s.split('/').next())
            .ok_or("launch: invalid fake endpoint")?
            .parse()
            .map_err(|_| "launch: invalid fake endpoint")?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err("launch: fake must bind 127.0.0.1".into());
        }
        Ok(address)
    }

    pub async fn control(&mut self, value: &Value) -> Result<(), String> {
        let host = self.host.as_mut().ok_or("host: not started")?;
        host.send(value).await?;
        host.expect("host.ack").await?;
        Ok(())
    }

    pub async fn complete_run(&mut self) -> Result<(), String> {
        let value = self
            .bridge
            .as_mut()
            .ok_or("bridge: not started")?
            .expect("bridge.run_completed")
            .await?;
        let id = value["run_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("bridge: missing completed run ID")?;
        self.run = Some(id.into());
        self.observe().await
    }

    pub async fn observe(&mut self) -> Result<(), String> {
        loop {
            self.refresh().await?;
            if !self.trace.partial() {
                return Ok(());
            }
            if self.bridge.as_mut().ok_or("trace: bridge not started")?.exited()? {
                return self.trace.finish();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn refresh(&mut self) -> Result<(), String> {
        let mut file = tokio::fs::File::open(&self.trace_path)
            .await
            .map_err(|_| "trace: missing bridge trace")?;
        if file.metadata().await.map_err(|_| "trace: cannot stat trace")?.len() < self.offset {
            return Err("trace: bridge truncated its trace".into());
        }
        file.seek(std::io::SeekFrom::Start(self.offset))
            .await
            .map_err(|_| "trace: cannot seek")?;
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "trace: cannot read")?;
        self.offset += u64::try_from(bytes.len()).map_err(|_| "trace: size overflow")?;
        self.trace.append(&bytes)?;
        if self.run.is_none() {
            self.run = self
                .trace
                .records
                .iter()
                .rev()
                .find(|r| r["event"] == "wyoming.run_ended")
                .and_then(|r| r["run_id"].as_str())
                .map(str::to_owned);
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = self.close_bridge().await;
        if let Some(live) = self.live.take() {
            live.shutdown().await;
        }
        if let Some(spoof) = self.spoof.take()
            && spoof.shutdown().await.is_err()
        {
            failures.push("cleanup: spoof shutdown failed");
        }
        if let Some(host) = self.host.take()
            && !close_process(host).await
        {
            failures.push("cleanup: host failed to close");
        }
        if self.remove_private().await.is_err() {
            failures.push("cleanup: cannot remove private observation");
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    async fn close_bridge(&mut self) -> Vec<&'static str> {
        let mut failures = Vec::new();
        if let Some(bridge) = self.bridge.take() {
            if !close_process(bridge).await {
                failures.push("cleanup: bridge failed to close");
            }
            if let Err(error) = self.refresh().await.and_then(|()| self.trace.finish()) {
                failures.push(if error.starts_with("trace: incomplete") {
                    "trace: incomplete final JSON line"
                } else {
                    "trace: invalid final trace"
                });
            }
        }
        failures
    }

    async fn remove_private(&self) -> Result<(), String> {
        let mut failed = false;
        for path in [&self.trace_path, &self.ui_path] {
            if path.is_file() && tokio::fs::remove_file(path).await.is_err() {
                failed = true;
            }
        }
        if failed {
            Err("cleanup: cannot remove private observation".into())
        } else {
            Ok(())
        }
    }
}

async fn close_process(mut process: Process) -> bool {
    if tokio::time::timeout(Duration::from_secs(5), process.close()).await == Ok(Ok(())) {
        return true;
    }
    process.terminate().await;
    false
}

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    // T3: a real owned child binds an ephemeral loopback socket, announces it
    // before listen(), and waits on an independent control connection.
    const CHILD: &str = r"
import json, socket, sys
with socket.socket() as host, socket.socket() as control:
    host.bind(('127.0.0.1', 0))
    control.connect(('127.0.0.1', int(sys.argv[1])))
    print(json.dumps({'event':'host.ready','address':'127.0.0.1:'+str(host.getsockname()[1])}), flush=True)
    control.sendall(str(host.getsockname()[1]).encode()+b'\n')
    if control.recv(1) == b'L':
        host.listen()
        control.sendall(b'L')
    sys.stdin.buffer.read()
";

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "lab-readiness-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    async fn announced(control: &mut tokio::net::TcpStream) -> std::net::SocketAddr {
        let mut port = vec![];
        loop {
            let byte = control.read_u8().await.unwrap();
            if byte == b'\n' {
                break;
            }
            port.push(byte);
        }
        format!("127.0.0.1:{}", String::from_utf8(port).unwrap())
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn t3_readiness_child_announces_before_listening() {
        child_startup(true).await;
    }

    #[tokio::test]
    async fn t3_readiness_child_never_listens_times_out_and_is_reaped() {
        child_startup(false).await;
    }

    async fn before_startup<T>(
        mut startup: std::pin::Pin<&mut impl std::future::Future<Output = Result<(), String>>>,
        signal: impl std::future::Future<Output = T>,
    ) -> T {
        tokio::select! {
            result = &mut startup => panic!("startup returned before listener signal: {result:?}"),
            value = signal => value,
        }
    }

    fn child_command(port: u16) -> Command {
        let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
        command.args(["-u", "-c", CHILD, &port.to_string()]);
        command
    }

    async fn release_listener(control: &mut tokio::net::TcpStream, listen: bool) {
        if listen {
            control.write_all(b"L").await.unwrap();
            assert_eq!(control.read_u8().await.unwrap(), b'L');
        } else {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(20)).await;
        }
    }

    fn check_startup(
        listen: bool,
        result: &Result<(), String>,
        endpoints: &[std::net::SocketAddr],
        endpoint: std::net::SocketAddr,
        next: usize,
    ) {
        if listen {
            assert_eq!(result, &Err("launch: fake Live startup failed".into()));
            assert_eq!(endpoints.len(), 2);
            assert_eq!(endpoints[0], endpoint);
            assert_eq!(next, 1);
        } else {
            assert_eq!(
                result,
                &Err(format!(
                    "launch: endpoint {endpoint} readiness timed out after 20000 ms"
                ))
            );
            assert_eq!(endpoints, &[]);
            assert_eq!(next, 0);
        }
    }

    async fn close_world(world: &mut World, root: &Path) -> Vec<std::net::SocketAddr> {
        let endpoints = world.endpoints.clone();
        let close = world.close().await;
        assert_eq!(world.close().await, Ok(()));
        assert_eq!(close, Ok(()));
        assert!(world.host.is_none());
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
        endpoints
    }

    #[tokio::test]
    async fn t3_readiness_cancel_keeps_owned_child_until_explicit_close() {
        let root = Root::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut command = child_command(listener.local_addr().unwrap().port());
        let probed = tokio::sync::Notify::new();
        let mut world = World::default();
        let next = std::cell::Cell::new(0);
        {
            let mut startup = std::pin::pin!(world.start_owned(
                &mut command,
                Path::new("not-launched"),
                &root.0,
                async {
                    next.set(next.get() + 1);
                    Err(echo_live::call::CallError::Signaling)
                },
                |address| {
                    probed.notify_one();
                    process::connect(address)
                },
            ));
            let (mut control, _) = before_startup(startup.as_mut(), async { listener.accept().await.unwrap() }).await;
            before_startup(startup.as_mut(), announced(&mut control)).await;
            before_startup(startup.as_mut(), probed.notified()).await;
        }
        assert_eq!(next.get(), 0);
        assert_eq!(world.endpoints, vec![]);
        assert!(!world.host.as_mut().unwrap().exited().unwrap());
        assert_eq!(close_world(&mut world, &root.0).await, vec![]);
    }

    async fn child_startup(listen: bool) {
        let root = Root::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut command = child_command(listener.local_addr().unwrap().port());
        let probed = tokio::sync::Notify::new();
        let mut world = World::default();
        let next = std::cell::Cell::new(0);
        let result;
        let endpoint;
        {
            let mut startup = std::pin::pin!(world.start_owned(
                &mut command,
                Path::new("not-launched"),
                &root.0,
                async {
                    next.set(next.get() + 1);
                    Err(echo_live::call::CallError::Signaling)
                },
                |address| {
                    probed.notify_one();
                    process::connect(address)
                }
            ));
            let (mut control, _) = before_startup(startup.as_mut(), async { listener.accept().await.unwrap() }).await;
            endpoint = before_startup(startup.as_mut(), announced(&mut control)).await;
            before_startup(startup.as_mut(), probed.notified()).await;
            assert_eq!(next.get(), 0);
            assert!(tokio::net::TcpStream::connect(endpoint).await.is_err());
            release_listener(&mut control, listen).await;
            result = startup.await;
        }
        if !listen {
            tokio::time::resume();
        }
        let endpoints = close_world(&mut world, &root.0).await;
        check_startup(listen, &result, &endpoints, endpoint, next.get());
        assert!(tokio::net::TcpStream::connect(endpoint).await.is_err());
    }
}
