//! T3 partial-startup fault at the Live factory boundary, with real acquired
//! host/Spoof handles. No fake substitutes for the resources being released.
use super::world::World;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
#[ignore = "T3 requires prebuilt pinned hassmic-host"]
async fn t3_failed_live_start_releases_the_already_running_host_and_spoof() {
    let root = std::env::temp_dir().join(format!("lab-partial-start-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut world = World::default();
    let result = world
        .start_with(python, std::path::Path::new("not-launched"), &root, async {
            Err(echo_live::call::CallError::Signaling)
        })
        .await;
    assert_eq!(result, Err("launch: fake Live startup failed".into()));
    assert_eq!(world.endpoints.len(), 2);
    for endpoint in &world.endpoints {
        let connection = tokio::net::TcpStream::connect(endpoint).await.unwrap();
        assert_eq!(connection.peer_addr().unwrap(), *endpoint);
        assert_eq!(endpoint.ip(), std::net::Ipv4Addr::LOCALHOST);
    }
    assert_eq!(world.close().await, Ok(()));
    assert_eq!(world.close().await, Ok(()));
    assert!(tokio::net::TcpStream::connect(world.endpoints[1]).await.is_err());
    if let Ok(mut stream) = tokio::net::TcpStream::connect(world.endpoints[0]).await {
        assert!(cfg!(windows) && stream.peer_addr().unwrap() == world.endpoints[0]);
        let _ = stream.write_all(b"{\"type\":\"describe\"}\n").await;
        let response = tokio::time::timeout(std::time::Duration::from_secs(1), stream.read_u8())
            .await
            .unwrap();
        assert!(response.is_err());
    }
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir(root).unwrap();
}

// T3 real child ownership checks; local stdin/pipe readiness, no sockets/sleeps.
#[test]
fn t3_drop_collects_after_runtime_teardown_and_dropped_close() {
    for dropped_close in [false, true] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (process, collected) = runtime.block_on(async {
            let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
            command.args([
                "-u",
                "-c",
                "import sys; print('{\"event\":\"ready\"}', flush=True); sys.stdin.buffer.read()",
            ]);
            let mut process = super::process::Process::start(&mut command).unwrap();
            process.expect("ready").await.unwrap();
            let collected = process.observe_collection();
            if dropped_close {
                let mut close = Box::pin(process.close());
                std::future::poll_fn(|cx| {
                    let _ = std::future::Future::poll(close.as_mut(), cx);
                    std::task::Poll::Ready(())
                })
                .await;
                drop(close);
            }
            (process, collected)
        });
        if dropped_close {
            // Drop on the runtime thread must not nest block_on or detach work.
            runtime.block_on(async {
                drop(process);
            });
            drop(runtime);
        } else {
            drop(runtime);
            drop(process);
        }
        let status = collected.try_recv().unwrap();
        assert_eq!(status.code(), Some(0));
    }
}

#[tokio::test]
async fn t3_close_collects_nonzero_and_is_idempotent() {
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args([
        "-u",
        "-c",
        "import sys; print('{\"event\":\"ready\"}', flush=True); sys.stdin.buffer.read(); sys.exit(7)",
    ]);
    let mut process = super::process::Process::start(&mut command).unwrap();
    process.expect("ready").await.unwrap();
    let collected = process.observe_collection();
    assert_eq!(process.close().await, Err("cleanup: owned child exited nonzero".into()));
    assert_eq!(process.close().await, Err("cleanup: owned child exited nonzero".into()));
    drop(process);
    assert_eq!(collected.try_recv().unwrap().code(), Some(7));
}

#[tokio::test]
async fn t3_grace_waits_for_real_exit_after_eof_acknowledgement() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args(["-u", "-c", "import socket,sys; s=socket.create_connection(('127.0.0.1',int(sys.argv[1]))); sys.stdin.buffer.read(); s.sendall(b'E'); assert s.recv(1)==b'R'", &listener.local_addr().unwrap().port().to_string()]);
    let mut process = super::process::Process::start(&mut command).unwrap();
    let collected = process.observe_collection();
    let (mut control, _) = listener.accept().await.unwrap();
    let mut close = Box::pin(process.close());
    tokio::select! {
        result = &mut close => panic!("returned before released exit: {result:?}"),
        value = control.read_u8() => assert_eq!(value.unwrap(), b'E'),
    }
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(close.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    control.write_all(b"R").await.unwrap();
    assert_eq!(close.await, Ok(()));
    assert_eq!(process.close().await, Ok(()));
    drop(process);
    assert_eq!(collected.try_recv().unwrap().code(), Some(0));
}

#[tokio::test]
async fn t3_cancelled_stop_keeps_its_deadline_and_collects_forced_exit() {
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args([
        "-u",
        "-c",
        "import threading; print('{\"event\":\"ready\"}',flush=True); threading.Event().wait()",
    ]);
    let mut process = super::process::Process::start(&mut command).unwrap();
    process.expect("ready").await.unwrap();
    let collected = process.observe_collection();
    tokio::time::pause();
    for elapsed in [0, 4999] {
        tokio::time::advance(std::time::Duration::from_millis(elapsed)).await;
        let mut close = Box::pin(process.close());
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(close.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(close);
    }
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert_eq!(process.close().await, Err("cleanup: owned child exited nonzero".into()));
    drop(process);
    let status = collected.try_recv().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9));
    }
    #[cfg(windows)]
    assert_eq!(status.code(), Some(1));
}

#[cfg(windows)]
struct WindowsLauncher {
    process: Option<super::process::Process>,
    observers: Vec<std::process::Child>,
    collected: std::sync::mpsc::Receiver<std::process::ExitStatus>,
    root: std::path::PathBuf,
}

#[cfg(windows)]
impl WindowsLauncher {
    fn command(function: &str) -> std::process::Command {
        let mut command = std::process::Command::new("python");
        command.args(["-u", "-c", &format!(
            "import sys; sys.path.insert(0,sys.argv[1]); from test_controls import {function}; {function}(*sys.argv[2:])"
        )]);
        command.arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/host_tests"));
        command
    }

    async fn start(mode: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = std::env::temp_dir().join(format!(
            "lab-win-owner-{}-{}",
            std::process::id(),
            listener.local_addr().unwrap().port()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut command = Self::command("native_launcher_fixture");
        command
            .arg(mode)
            .arg(listener.local_addr().unwrap().port().to_string())
            .arg(root.join("owned.log"));
        let mut process = super::process::Process::start(&mut command.into()).unwrap();
        let collected = process.observe_collection();
        let mut fixture = Self {
            process: Some(process),
            observers: vec![],
            collected,
            root,
        };
        let phases = if mode == "translation-stall" { 1 } else { 2 };
        for _ in 0..phases {
            let (control, _) = tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut reader = tokio::io::BufReader::new(control);
            let mut line = String::new();
            tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            let mut command = Self::command("native_handle_observer");
            command.arg(value["pid"].to_string());
            let mut observer = command
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let value = Self::observation(&mut observer);
            assert_eq!(value["event"], "observer.ready", "{value}");
            fixture.observers.push(observer);
            reader.get_mut().write_all(b"G").await.unwrap();
        }
        fixture
    }

    fn observation(observer: &mut std::process::Child) -> serde_json::Value {
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(observer.stdout.as_mut().unwrap()),
            &mut line,
        )
        .unwrap();
        assert!(
            !line.is_empty(),
            "native observer EOF; observed status={:?}",
            observer.try_wait().unwrap()
        );
        serde_json::from_str(&line).unwrap()
    }

    fn verify(&mut self, forced: bool) {
        assert_eq!(self.collected.try_recv().unwrap().code(), Some(i32::from(forced)));
        let last = self.observers.len() - 1;
        for (index, observer) in self.observers.iter_mut().enumerate() {
            std::io::Write::write_all(observer.stdin.as_mut().unwrap(), b"collect\n").unwrap();
            let value = Self::observation(observer);
            assert_eq!(value["event"], "observer.collected", "{value}");
            assert_eq!(value["owned_survivors"], 0);
            assert_eq!(value["status"], u32::from(forced && index == last));
            assert!(observer.wait().unwrap().success());
        }
        let audit = std::fs::read_to_string(self.root.join("owned.log")).unwrap();
        assert_eq!(
            audit.lines().filter(|line| line.ends_with(" force")).count(),
            usize::from(forced)
        );
        std::io::Write::write_all(
            &mut std::io::stdout(),
            format!(
                "N-WIN Rust Drop: wrapper/descendant statuses collected, owned_survivors=0, force_requests={}\n",
                usize::from(forced)
            )
            .as_bytes(),
        )
        .unwrap();
    }
}

#[cfg(windows)]
impl Drop for WindowsLauncher {
    fn drop(&mut self) {
        drop(self.process.take());
        for observer in &mut self.observers {
            drop(observer.stdin.take());
            let _ = observer.wait();
        }
        let _ = std::fs::remove_file(self.root.join("owned.log"));
        let _ = std::fs::remove_dir(&self.root);
    }
}

#[cfg(windows)]
fn windows_launcher_drop(mode: &str, cancelled: bool, runtime_gone: bool) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fixture = runtime.block_on(WindowsLauncher::start(mode));
    if cancelled {
        runtime.block_on(async {
            let mut close = Box::pin(fixture.process.as_mut().unwrap().close());
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(close.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        });
    }
    if runtime_gone {
        drop(runtime);
        drop(fixture.process.take());
        fixture.verify(mode.ends_with("stall"));
    } else {
        runtime.block_on(async {
            drop(fixture.process.take());
            fixture.verify(mode.ends_with("stall"));
        });
    }
}

#[test]
#[cfg(windows)]
fn t3_windows_translation_drop_after_runtime_teardown() {
    windows_launcher_drop("translation-stall", false, true);
}

#[test]
#[cfg(windows)]
fn t3_windows_running_cancelled_close_then_drop_on_runtime() {
    windows_launcher_drop("running-stall", true, false);
}

#[test]
#[cfg(windows)]
fn t3_windows_normal_drop_after_runtime_teardown() {
    windows_launcher_drop("normal", false, true);
}

#[test]
#[cfg(windows)]
fn t3_windows_running_cancelled_close_drop_after_runtime_teardown() {
    windows_launcher_drop("running-stall", true, true);
}

#[tokio::test]
#[cfg(windows)]
async fn t3_windows_translation_cancelled_close_resumes_and_collects() {
    let mut fixture = WindowsLauncher::start("translation-stall").await;
    let process = fixture.process.as_mut().unwrap();
    {
        let mut close = Box::pin(process.close());
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(close.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert_eq!(process.close().await, Err("cleanup: owned child exited nonzero".into()));
    assert_eq!(process.close().await, Err("cleanup: owned child exited nonzero".into()));
    drop(fixture.process.take());
    fixture.verify(true);
}

#[cfg(windows)]
fn require_prebuilt_host() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let pin = std::fs::read_to_string(repo.join("hassmic.pin")).unwrap();
    let commit = pin.lines().find_map(|line| line.strip_prefix("commit=")).unwrap();
    assert!(
        repo.join(".cache/hassmic-host")
            .join(commit)
            .join("hassmic-host")
            .is_file(),
        "BLOCKED R-WSL: prebuilt pinned host is unavailable; do not build inside the test"
    );
}

#[cfg(windows)]
async fn real_wsl_cleanup(interrupted: bool, busy: bool) {
    require_prebuilt_host();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(".cache")
        .join(format!(
            "lab-real-wsl-{}-{}",
            std::process::id(),
            reservation.local_addr().unwrap().port()
        ));
    std::fs::create_dir(&root).unwrap();
    let mut command = WindowsLauncher::command("real_wsl_launcher_fixture");
    command.arg(root.file_name().unwrap());
    let mut process = super::process::Process::start(&mut command.into()).unwrap();
    let collected = process.observe_collection();
    let fixture = WindowsLauncher {
        process: None,
        observers: vec![],
        collected,
        root,
    };
    let (ready, identity) = owned_handshake(&mut process).await;
    let address: std::net::SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
    assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
    assert!(identity["pid"].as_u64().unwrap() > 0);
    if busy {
        process.send(&serde_json::json!({"action":"registered"})).await.unwrap();
        process.expect("guest.command.entered").await.unwrap();
    }
    if interrupted {
        drop(process);
    } else {
        assert_eq!(process.close().await, Ok(()));
        assert_eq!(
            process.expect("host.closed").await.unwrap(),
            serde_json::json!({"event":"host.closed"})
        );
        drop(process);
    }
    assert_eq!(fixture.collected.try_recv().unwrap().code(), Some(0));
    let (witness, exit) = guest_cleanup_witness(&fixture, &identity);
    std::io::Write::write_all(
        &mut std::io::stdout(),
        format!(
            "R-WSL host.ready then guest.owned; interrupted={interrupted}, busy={busy}: guest={witness}, observer={exit}, listener_owner_collected=true\n"
        )
        .as_bytes(),
    )
    .unwrap();
}

#[cfg(windows)]
async fn owned_handshake(process: &mut super::process::Process) -> (serde_json::Value, serde_json::Value) {
    let deadline = super::process::readiness_deadline();
    let ready = tokio::time::timeout_at(deadline, process.line())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready["event"], "host.ready", "{ready}");
    let identity = tokio::time::timeout_at(deadline, process.line())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(identity["event"], "guest.owned", "{identity}");
    (ready, identity)
}

#[cfg(windows)]
fn guest_cleanup_witness(
    fixture: &WindowsLauncher,
    identity: &serde_json::Value,
) -> (serde_json::Value, serde_json::Value) {
    let exit_path = fixture.root.join("guest.json.exit");
    let exit: serde_json::Value = serde_json::from_slice(&std::fs::read(&exit_path).unwrap()).unwrap();
    assert_eq!(exit["owned_survivors"], 0);
    assert_eq!(exit["fixture_cleaned"], true);
    std::fs::remove_file(exit_path).unwrap();
    let witness: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.root.join("guest.json")).unwrap()).unwrap();
    assert_eq!(witness["pid"], identity["pid"]);
    assert_eq!(witness["closed"], true);
    assert!(witness["status"].as_i64().is_some());
    std::fs::remove_file(fixture.root.join("guest.json")).unwrap();
    (witness, exit)
}

#[tokio::test]
#[cfg(windows)]
#[ignore = "T3 requires prebuilt pinned hassmic-host and native WSL guest observer"]
async fn t3_windows_wsl_owned_cleanup_normal() {
    real_wsl_cleanup(false, false).await;
}

#[tokio::test]
#[cfg(windows)]
#[ignore = "T3 requires prebuilt pinned hassmic-host and native WSL guest observer"]
async fn t3_windows_wsl_owned_cleanup_interrupted() {
    real_wsl_cleanup(true, false).await;
}

#[tokio::test]
#[cfg(windows)]
#[ignore = "T3 requires prebuilt pinned hassmic-host and native WSL guest observer"]
async fn t3_windows_wsl_owned_cleanup_busy_interrupted() {
    real_wsl_cleanup(true, true).await;
}
