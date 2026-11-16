use std::path::Path;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub struct Process {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

impl Process {
    pub fn start(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "launch: cannot spawn owned process")?;
        let input = child.stdin.take();
        let output = child.stdout.take().ok_or("launch: missing owned stdout")?;
        Ok(Self { child, input, output })
    }

    pub async fn line(&mut self) -> Result<Value, String> {
        let mut bytes = Vec::new();
        loop {
            let byte = self
                .output
                .read_u8()
                .await
                .map_err(|_| "process: exited before observation")?;
            if bytes.len() >= 4096 {
                return Err("process: oversized observation".into());
            }
            if byte == b'\n' {
                return serde_json::from_slice(&bytes).map_err(|_| "process: malformed observation".into());
            }
            bytes.push(byte);
        }
    }

    pub async fn expect(&mut self, event: &str) -> Result<Value, String> {
        let value = self.line().await?;
        if value["event"] != event {
            return Err("process: unexpected observation".into());
        }
        Ok(value)
    }

    pub async fn send(&mut self, value: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| "process: cannot encode control")?;
        bytes.push(b'\n');
        let input = self.input.as_mut().ok_or("process: control closed")?;
        input
            .write_all(&bytes)
            .await
            .map_err(|_| "process: cannot write control")?;
        input.flush().await.map_err(|_| "process: cannot flush control".into())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        drop(self.input.take());
        let status = self
            .child
            .wait()
            .await
            .map_err(|_| "cleanup: cannot reap owned child")?;
        if !status.success() {
            return Err("cleanup: owned child exited nonzero".into());
        }
        Ok(())
    }

    pub async fn terminate(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    pub fn exited(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|s| s.is_some())
            .map_err(|_| "process: cannot observe child exit".into())
    }
}

pub fn host(python: &str) -> Command {
    let mut command = Command::new(python);
    command
        .arg("-u")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/host.py"));
    command
}

// An announcement proves child-side readiness, not caller-side reachability
// (notably across WSL forwarding). Share one budget with host observation.
const READINESS: std::time::Duration = std::time::Duration::from_secs(20);

pub(super) fn readiness_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + READINESS
}

pub(super) async fn connect(address: std::net::SocketAddr) -> bool {
    tokio::net::TcpStream::connect(address).await.is_ok()
}

pub(super) async fn admit_host<O, P, F>(
    endpoints: &mut Vec<std::net::SocketAddr>,
    observation: O,
    deadline: tokio::time::Instant,
    probe: P,
) -> Result<std::net::SocketAddr, String>
where
    O: std::future::Future<Output = Result<Value, String>>,
    P: FnMut(std::net::SocketAddr) -> F,
    F: std::future::Future<Output = bool>,
{
    let value = tokio::time::timeout_at(deadline, observation)
        .await
        .map_err(|_| "launch: hassmic-host readiness timed out after 20000 ms")??;
    let address = value["address"].as_str().ok_or("launch: missing host address")?;
    let address: std::net::SocketAddr = address.parse().map_err(|_| "launch: invalid host address")?;
    if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
        return Err("launch: host must target 127.0.0.1".into());
    }
    admit(endpoints, address, deadline, probe).await?;
    Ok(address)
}

pub(super) async fn admit<P, F>(
    endpoints: &mut Vec<std::net::SocketAddr>,
    address: std::net::SocketAddr,
    deadline: tokio::time::Instant,
    mut probe: P,
) -> Result<(), String>
where
    P: FnMut(std::net::SocketAddr) -> F,
    F: std::future::Future<Output = bool>,
{
    let ready = async {
        while tokio::time::Instant::now() < deadline {
            if probe(address).await && tokio::time::Instant::now() < deadline {
                return true;
            }
            // Retry only after a completed failed probe, yielding to the child
            // and cancellation instead of using a fixed sleep as readiness.
            tokio::task::yield_now().await;
        }
        false
    };
    if tokio::time::timeout_at(deadline, ready).await != Ok(true) {
        return Err(format!("launch: endpoint {address} readiness timed out after 20000 ms"));
    }
    endpoints.push(address);
    Ok(())
}

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use std::{
        cell::Cell,
        future::{Future, pending, ready},
    };
    use tokio::time::{Instant, advance};

    fn address() -> std::net::SocketAddr {
        "127.0.0.1:32123".parse().unwrap()
    }

    fn observation() -> Value {
        serde_json::json!({"event":"host.ready", "address":address().to_string()})
    }

    fn timeout() -> String {
        "launch: endpoint 127.0.0.1:32123 readiness timed out after 20000 ms".into()
    }

    async fn pending_once(future: std::pin::Pin<&mut impl Future>) {
        let mut future = future;
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_ready_publishes_once_before_next_stage() {
        let mut endpoints = vec![];
        let probes = Cell::new(0);
        let next = Cell::new(0);
        let actual = admit_host(&mut endpoints, ready(Ok(observation())), readiness_deadline(), |a| {
            assert_eq!(a, address());
            probes.set(probes.get() + 1);
            ready(true)
        })
        .await;
        if actual.is_ok() {
            next.set(next.get() + 1);
        }
        assert_eq!(actual, Ok(address()));
        assert_eq!(endpoints, vec![address()]);
        assert_eq!((probes.get(), next.get()), (1, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_delayed_probe_blocks_publication_and_next_stage() {
        let mut endpoints = vec![];
        let probes = Cell::new(0);
        let next = Cell::new(0);
        let release = tokio::sync::Notify::new();
        let mut startup = Box::pin(async {
            admit_host(&mut endpoints, ready(Ok(observation())), readiness_deadline(), |_| {
                probes.set(probes.get() + 1);
                let attempt = probes.get();
                let release = &release;
                async move {
                    if attempt == 1 {
                        return false;
                    }
                    release.notified().await;
                    true
                }
            })
            .await?;
            next.set(next.get() + 1);
            Ok::<(), String>(())
        });
        pending_once(startup.as_mut()).await;
        pending_once(startup.as_mut()).await;
        assert_eq!((probes.get(), next.get()), (2, 0));
        release.notify_one();
        assert_eq!(startup.await, Ok(()));
        assert_eq!(endpoints, vec![address()]);
        assert_eq!(next.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_timeout_bounds_refused_and_stalled_probes() {
        for stalled in [false, true] {
            let mut endpoints = vec![];
            let next = Cell::new(0);
            let mut startup = Box::pin(async {
                admit_host(
                    &mut endpoints,
                    ready(Ok(observation())),
                    readiness_deadline(),
                    |_| async {
                        if stalled {
                            pending::<()>().await;
                        }
                        false
                    },
                )
                .await?;
                next.set(next.get() + 1);
                Ok::<(), String>(())
            });
            pending_once(startup.as_mut()).await;
            advance(READINESS).await;
            assert_eq!(startup.await, Err(timeout()));
            assert_eq!(endpoints, vec![]);
            assert_eq!(next.get(), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_expired_deadline_never_probes() {
        let mut endpoints = vec![];
        let probes = Cell::new(0);
        assert_eq!(
            admit_host(&mut endpoints, ready(Ok(observation())), Instant::now(), |_| {
                probes.set(probes.get() + 1);
                ready(true)
            })
            .await,
            Err(timeout())
        );
        assert_eq!(endpoints, vec![]);
        assert_eq!(probes.get(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_missing_announcement_uses_same_budget() {
        let mut endpoints = vec![];
        let mut startup = Box::pin(admit_host(&mut endpoints, pending(), readiness_deadline(), |_| {
            ready(true)
        }));
        pending_once(startup.as_mut()).await;
        advance(READINESS).await;
        assert_eq!(
            startup.await,
            Err("launch: hassmic-host readiness timed out after 20000 ms".into())
        );
        assert_eq!(endpoints, vec![]);
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_invalid_observations_preserve_errors_without_probes() {
        let cases = [
            (Ok(serde_json::json!({})), "launch: missing host address"),
            (
                Ok(serde_json::json!({"address":"invalid"})),
                "launch: invalid host address",
            ),
            (
                Ok(serde_json::json!({"address":"127.0.0.1:0"})),
                "launch: host must target 127.0.0.1",
            ),
            (
                Ok(serde_json::json!({"address":"192.0.2.1:123"})), // lint-rules: allow-literal inert rejected address; never probed
                "launch: host must target 127.0.0.1",
            ),
            (
                Err("process: malformed observation".into()),
                "process: malformed observation",
            ),
            (
                Err("process: exited before observation".into()),
                "process: exited before observation",
            ),
            (
                Err("process: unexpected observation".into()),
                "process: unexpected observation",
            ),
        ];
        for (observation, expected) in cases {
            let mut endpoints = vec![];
            let probes = Cell::new(0);
            assert_eq!(
                admit_host(&mut endpoints, ready(observation), readiness_deadline(), |_| {
                    probes.set(probes.get() + 1);
                    ready(true)
                })
                .await,
                Err(expected.into())
            );
            assert_eq!(endpoints, vec![]);
            assert_eq!(probes.get(), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_cancellation_does_not_publish_or_release_callers_owner() {
        struct Owner<'a>(&'a Cell<usize>);
        impl Drop for Owner<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let releases = Cell::new(0);
        let owner = Owner(&releases);
        let mut endpoints = vec![];
        {
            let mut startup = Box::pin(admit_host(
                &mut endpoints,
                ready(Ok(observation())),
                readiness_deadline(),
                |_| pending(),
            ));
            pending_once(startup.as_mut()).await;
        }
        assert_eq!(endpoints, vec![]);
        assert_eq!(releases.get(), 0);
        drop(owner);
        assert_eq!(releases.get(), 1);
    }
}
