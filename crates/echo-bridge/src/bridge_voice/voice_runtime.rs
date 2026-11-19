//! Presentation lifetime encloses authenticated device and conversation cleanup.
use super::{HostOptions, ServeOptions, voice_host::with_capture, voice_view::View};
use crate::{Error, bridge_health::Health, bridge_runtime::bridge_screen::Screen, bridge_trace::Trace};
use echo_config::{LiveMode, ScreenConfig};
use echo_link::LinkConnector;
use echo_media::MediaPeer;
use std::{cell::Cell, future::Future, net::SocketAddr, path::Path};
use tokio::sync::watch;

pub struct RuntimeOptions<'a> {
    pub host: HostOptions<'a>,
    pub voice: ServeOptions<'a>,
    pub screen: Option<&'a ScreenConfig>,
    pub health: SocketAddr,
    pub trace: &'a Path,
    pub delegation: Option<&'a echo_config::BridgeConfig>,
    pub utc_day: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    pub mcp_epoch: Option<u64>,
}

impl RuntimeOptions<'_> {
    /// Device stalled-progress protection exceeds either waiting or active work.
    /// It is separate from controller idle policy and between-wakes call reuse.
    pub(super) fn capture_failsafe_seconds(&self) -> u32 {
        let Some(config) = self.delegation else { return 30 };
        let seconds = u64::from(config.voice.session_max_seconds)
            .max(self.voice.session.response_timeout.as_secs())
            .saturating_add(self.voice.session.played_timeout.as_secs());
        u32::try_from(seconds.min(i32::MAX as u64)).unwrap_or(i32::MAX as u32)
    }
}

/// Compose the Codex voice service with the existing screen, health and trace
/// owners. Authentication precedes device readiness. Only negotiated media can
/// publish Connected; played settlement precedes Ready. All device/call/process
/// cleanup finishes while presentation remains owned.
///
/// Configured delegation discovers MCP tools after authentication and retains
/// policy, consent and the effect ledger across calls. The service monitors
/// the nominated kill file; the caller supplies a peer factory.
///
/// # Errors
/// Startup, service, publication and cleanup failures use fixed categories.
/// Authorization failures publish the existing relink instruction and keep the
/// screen available until cancellation; no implicit login or restart occurs.
pub async fn execute<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    mut peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
) -> Result<(), Error> {
    let mut trace = Trace::open(
        options.trace,
        match options.host.mode {
            LiveMode::Chatgpt => "chatgpt",
            LiveMode::ApiKey => "api_key",
        },
    )?;
    super::prompts::publish(&mut trace)?;
    let mut screen = Screen::bind(options.screen).await?;
    let health = match Health::bind(options.health).await {
        Ok(health) => health,
        Err(error) => {
            let _ = close_screen(&mut screen, &mut trace, false).await;
            return Err(error);
        }
    };
    screen.supervise_actions(health.state.subscribe());
    let mut view = View::new(&screen, &health, &mut trace);
    let requested = Cell::new(false);
    let result = work(options, connector, &mut peer, cancelled, &mut view, &requested).await;
    health.close().await;
    let health_receipt = trace.record("health.released", None, serde_json::json!({}));
    // A trace failure must not skip the remaining presentation cleanup.
    let cleanup = close_screen(
        &mut screen,
        &mut trace,
        !requested.get() && result != Err(Error::Control),
    )
    .await;
    result.and(cleanup).and(health_receipt)
}

async fn close_screen(screen: &mut Screen, trace: &mut Trace, graceful: bool) -> Result<(), Error> {
    let acquired = screen.address().is_some();
    let dropped = if graceful {
        screen.close_gracefully().await?
    } else {
        screen.close().await?;
        0
    };
    if acquired {
        trace.record("screen.released", None, serde_json::json!({"dropped_clients":dropped}))?;
    }
    Ok(())
}

async fn work<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    view: &mut View<'_>,
    requested: &Cell<bool>,
) -> Result<(), Error> {
    view.startup()?;
    let cancelled = async {
        cancelled.await;
        requested.set(true);
    };
    tokio::pin!(cancelled);
    let termination = super::voice_kill::Termination::default();
    let kill_path = options.voice.kill_path;
    let failsafe_seconds = options.capture_failsafe_seconds();
    let result = with_capture(
        options.host,
        connector,
        failsafe_seconds,
        cancelled.as_mut(),
        async |io, client, stopping| {
            let mut tools = options.delegation.map(|config| super::voice_tools::Tools::new(config, options.utc_day));
            let setup = async {
                if let (Some(config), Some(tools)) = (options.delegation, &mut tools) {
                    view.open_tools(tools, config, options.mcp_epoch).await?;
                }
                Ok::<_, Error>(())
            };
            let ready = tokio::select! {
                biased;
                () = stopped(stopping.clone()) => Err(Error::Control),
                result = tokio::time::timeout(std::time::Duration::from_secs(15), setup) => result.unwrap_or(Err(Error::Deadline)),
            };
            let setup_trace = view.setup_trace();
            let result = match ready {
                Ok(()) => super::voice_admission::service(client, io, options.voice, peer, stopped(stopping), |notice| {
                termination.observe(&notice);
                view.observe(notice)
                }, (tools.as_mut(), Some(setup_trace), options.delegation.is_some_and(|config| config.wake_earcon), Some(&termination))).await,
                Err(error) => Err(error),
            };
            termination.returned(&result, requested.get());
            if let Some(tools) = &mut tools { tools.close().await; }
            result
        },
    )
    .await;
    // with_device has now awaited call/media, device and process shutdown.
    let result = termination.finish(result, requested.get(), kill_path, &mut |notice| view.observe(notice));
    match result {
        Err(error) => {
            view.failed(&error).await?;
            if error == Error::Authorization && !requested.get() {
                cancelled.await;
            }
            Err(error)
        }
        Ok(()) => view.closed(if requested.get() { "shutdown" } else { "stopped" }),
    }
}

async fn stopped(mut stopping: watch::Receiver<bool>) {
    loop {
        if *stopping.borrow_and_update() || stopping.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    // T1: real terminal arbitration and trace projection, memory-only cleanup
    // barriers and a private durable marker. No timer or socket is needed.
    #[tokio::test(start_paused = true)]
    async fn wake_earcon_kill_shutdown_ordering() {
        for order in [
            "kill_first",
            "eof_first",
            "both_ready",
            "kill_cleared",
            "shutdown",
            "failure",
            "mute",
        ] {
            terminal_order(order).await;
        }
    }

    async fn terminal_order(order: &str) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kill");
        let trace_path = directory.path().join("trace");
        let mut trace = Trace::open(&trace_path, "api_key").unwrap();
        let screen = Screen::bind(None).await.unwrap();
        let health = Health::offline();
        let mut view = View::new(&screen, &health, &mut trace);
        let termination = super::super::voice_kill::Termination::default();
        let requested = Cell::new(false);
        let (entered, cleanup_entered) = tokio::sync::oneshot::channel();
        let (release, cleanup_released) = tokio::sync::oneshot::channel();
        let work = async {
            observe_terminal(&termination, &mut view, super::super::ServeNotice::Listening);
            let result = interrupt(order, &path, &termination, &mut view).await;
            requested.set(order == "eof_first" || order == "both_ready" || order == "shutdown");
            termination.returned(&result, requested.get());
            entered.send(()).unwrap();
            cleanup_released.await.unwrap();
            termination.finish(result, requested.get(), Some(&path), &mut |notice| view.observe(notice))
        };
        let signals = async {
            cleanup_entered.await.unwrap();
            if order == "eof_first" {
                std::fs::write(&path, b"").unwrap();
            } else if order == "kill_cleared" {
                std::fs::remove_file(&path).unwrap();
            }
            requested.set(true);
            release.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(work, signals);
        assert_eq!(
            result,
            if order == "shutdown" {
                Ok(())
            } else if order == "failure" {
                Err(Error::Wyoming)
            } else {
                Err(Error::Control)
            },
            "{order}"
        );
        terminal_receipts(&trace_path, order);
        // Clear/recovery starts a new run rather than poisoning future EOF.
        let _ = std::fs::remove_file(&path);
        observe_terminal(&termination, &mut view, super::super::ServeNotice::Listening);
        assert_eq!(
            termination.finish(Err(Error::Control), true, Some(&path), &mut |notice| view
                .observe(notice)),
            Ok(())
        );
    }

    fn terminal_receipts(trace_path: &Path, order: &str) {
        let rows: Vec<Value> = std::fs::read_to_string(trace_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows.iter().filter(|row| row["event"] == "kill.enabled").count(),
            usize::from(matches!(
                order,
                "kill_first" | "eof_first" | "both_ready" | "kill_cleared"
            )),
            "{order}"
        );
        assert!(!rows.iter().any(|row| matches!(
            row["event"].as_str(),
            Some("wake.earcon_played" | "reply.played" | "bridge.run_completed")
        )));
    }

    fn observe_terminal(
        termination: &super::super::voice_kill::Termination,
        view: &mut View<'_>,
        notice: super::super::ServeNotice,
    ) {
        termination.observe(&notice);
        view.observe(notice).unwrap();
    }

    async fn interrupt(
        order: &str,
        path: &Path,
        termination: &super::super::voice_kill::Termination,
        view: &mut View<'_>,
    ) -> Result<(), Error> {
        use super::super::{Notice, ServeNotice, SessionNotice};
        if matches!(order, "kill_first" | "kill_cleared" | "both_ready") {
            std::fs::write(path, b"").unwrap();
            let killed = Cell::new(false);
            let gate = super::super::voice_kill::Gate::new(Some(path));
            gate.cancel(
                async {
                    if order != "both_ready" {
                        std::future::pending::<()>().await;
                    }
                },
                &killed,
            )
            .await;
            assert!(killed.get(), "{order}: ready kill must beat EOF");
            if order == "kill_first" {
                observe_terminal(termination, view, ServeNotice::Kill(true));
            } else {
                termination.trip();
            }
        } else if order == "mute" {
            observe_terminal(
                termination,
                view,
                ServeNotice::Session(SessionNotice::Run(Notice::Device(echo_link::LinkEvent::Mute(true)))),
            );
        }
        Err(if order == "failure" {
            Error::Wyoming
        } else {
            Error::Control
        })
    }

    async fn screen(directory: &Path) -> Screen {
        let token = directory.join("pairing");
        std::fs::write(&token, "synthetic-screen-pair").unwrap();
        Screen::bind(Some(&ScreenConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            pairing_token_file: token,
        }))
        .await
        .unwrap()
    }

    async fn request(screen: &Screen, request: &str) -> BufReader<TcpStream> {
        let mut stream = TcpStream::connect(screen.address().unwrap()).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            headers.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        assert!(headers.starts_with("HTTP/1.1 200 OK") || headers.starts_with("HTTP/1.1 100 Continue"));
        reader
    }

    // T3: actual loopback SSE, production error publication and release trace.
    #[tokio::test]
    async fn t3_uncancelled_terminal_error_reaches_screen_before_release() {
        for _ in 0..100 {
            let directory = tempfile::tempdir().unwrap();
            let mut screen = screen(directory.path()).await;
            let path = directory.path().join("trace");
            let mut trace = Trace::open(&path, "api_key").unwrap();
            let mut stream = request(
                &screen,
                "GET /events?token=synthetic-screen-pair HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await;
            let health = Health::offline();
            View::new(&screen, &health, &mut trace)
                .failed(&Error::Unavailable)
                .await
                .unwrap();
            close_screen(&mut screen, &mut trace, true).await.unwrap();
            let mut tail = String::new();
            stream.read_to_string(&mut tail).await.unwrap();
            let frame = tail.lines().find_map(|line| line.strip_prefix("data: ")).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(frame).unwrap(),
                json!({
                    "seq":1, "state":{"state":"error", "message":"Conversation unavailable. Try another wake.", "relink":false}
                })
            );
            let rows: Vec<Value> = std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows[0]["event"], "bridge.error");
            assert_eq!(rows[1]["event"], "screen.released");
            assert_eq!(rows[1]["dropped_clients"], 0);
        }
    }

    // T3: 100 Continue proves this owned HTTP handler is awaiting a real body.
    // Unsent body bytes cannot be drained; cancellation must bypass that wait.
    #[tokio::test(start_paused = true)]
    async fn t3_screen_release_bounds_stalled_client_and_cancellation_skips_drain() {
        for graceful in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let mut screen = screen(directory.path()).await;
            let path = directory.path().join("trace");
            let mut trace = Trace::open(&path, "api_key").unwrap();
            let mut stream = request(&screen, "POST /actions?token=synthetic-screen-pair HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n").await;
            let start = tokio::time::Instant::now();
            close_screen(&mut screen, &mut trace, graceful).await.unwrap();
            assert_eq!(
                start.elapsed(),
                std::time::Duration::from_millis(if graceful { 250 } else { 0 })
            );
            let row: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(row["event"], "screen.released");
            assert_eq!(row["dropped_clients"], usize::from(graceful));
            let mut tail = Vec::new();
            stream.read_to_end(&mut tail).await.unwrap();
            assert_eq!(tail, [] as [u8; 0]);
            assert!(screen.address().is_none());
        }
    }
}
