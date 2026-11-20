//! T3 installed setup sequence with real child processes and local health HTTP.
//! Provider prompts are protected by guided.rs; this protects their sequencing
//! with lifecycle, health admission and paired fresh wake/run observations.
use super::*;

fn prepare(fixture: &Fixture, health: u16) -> Result<TcpListener> {
    let screen = std::net::TcpListener::bind("127.0.0.1:0")?;
    screen.set_nonblocking(true)?;
    let port = screen.local_addr()?.port();
    fs::write(fixture.0.join("stop-reply"), "echo-install: stopped\r\n")?;
    for verb in ["ensure-signin", "signin", "link"] {
        fs::write(
            fixture.0.join(format!("{verb}-reply")),
            format!("echo-install: {verb} completed\r\n"),
        )?;
    }
    fs::write(fixture.0.join("setup-pairing"), "SYNTHETIC-PAIR\n")?;
    let replies: Vec<_> = (0..8).flat_map(|_| [port.to_string(), health.to_string()]).collect();
    fs::write(fixture.0.join("forward-replies.json"), serde_json::to_vec(&replies)?)?;
    fs::write(fixture.0.join("forwards.json"), "[]")?;
    fs::write(fixture.0.join("forward-targets.json"), "{}")?;
    fs::write(
        fixture.0.join("confirm-cursor"),
        format!("1:2:0:{}\n1:3:0:{}\n", "a".repeat(64), "b".repeat(64)), // lint-rules: allow-literal inert opaque log cursors
    )?;
    fs::write(
        fixture.0.join("confirm-evidence"),
        "wake=pending\nlink=pending\nrun=pending\n",
    )?;
    Ok(TcpListener::from_std(screen)?)
}

async fn health_server(listener: TcpListener, screen: TcpListener, states: Vec<(bool, bool)>) -> Result {
    health_settings_server(listener, screen, states, "link_required").await
}

async fn health_settings_server(
    listener: TcpListener,
    screen: TcpListener,
    states: Vec<(bool, bool)>,
    link_state: &str,
) -> Result {
    let mut states = states.into_iter();
    while let Some(&(relink, kill)) = states.as_slice().first() {
        let (mut socket, _) = tokio::select! {
            accepted = listener.accept() => accepted?,
            accepted = screen.accept() => accepted?,
        };
        let Some(request) = setup_request(&mut socket).await? else {
            continue;
        };
        let settings = request.starts_with("GET /settings/v1?token=SYNTHETIC-PAIR ");
        let body = if settings {
            serde_json::json!({"version":1,"link_status":link_state,"ignored":"SYNTHETIC-TOKEN"}).to_string()
        } else {
            assert_eq!(request.lines().next(), Some("GET /healthz HTTP/1.1"));
            states.next();
            serde_json::json!({"status":"running", "wyoming":"connected", "live":"idle", "relink_required":relink, "kill_switch":kill}).to_string()
        };
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        if settings && !matches!(link_state, "linked" | "link_required") {
            break;
        }
    }
    Ok(())
}

async fn setup_request(socket: &mut TcpStream) -> Result<Option<String>> {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        if socket.read_exact(&mut byte).await.is_err() {
            // An existing account can finish and cancel its observer before
            // the HTTP request is complete. That must not kill the fixture.
            return Ok(None);
        }
        request.push(byte[0]);
    }
    let request = String::from_utf8(request)?;
    if request.starts_with("GET /events?token=SYNTHETIC-PAIR ") {
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await;
        let mut byte = [0];
        let _ = socket.read(&mut byte).await;
        return Ok(None);
    }
    Ok(Some(request))
}

fn operations(fixture: &Fixture) -> Result<Vec<String>> {
    Ok(fixture
        .calls()?
        .into_iter()
        .filter_map(|args| {
            args.get(3)
                .and_then(|arg| arg.strip_prefix("sh -s -- --device "))
                .map(str::to_owned)
        })
        .filter(|operation| !operation.starts_with("setup-"))
        .collect())
}

fn no_forwards(fixture: &Fixture) -> Result {
    let remaining: Vec<String> = serde_json::from_slice(&fs::read(fixture.0.join("forwards.json"))?)?;
    assert_eq!(remaining, Vec::<String>::new());
    assert!(
        fixture
            .calls()?
            .iter()
            .skip(1)
            .all(|args| args[..2] == ["-s", "SYNTHETIC-DEVICE"])
    );
    Ok(())
}

#[tokio::test]
async fn t3_installed_setup_orders_signin_required_link_and_fresh_confirmation_on_selected_device() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    fs::write(fixture.0.join("devices"), "SYNTHETIC-OTHER\tdevice\n")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let screen = prepare(&fixture, listener.local_addr()?.port())?;
    let server = tokio::spawn(health_server(listener, screen, vec![(false, false); 2]));
    let mut prompts = 0;
    let mut instructions = 0;
    connection
        .guide(
            100,
            |_| {
                prompts += 1;
                Ok(())
            },
            || {
                instructions += 1;
                let calls = operations(&fixture).map_err(|_| Failure("output_failed"))?;
                assert_eq!(
                    calls,
                    ["stop", "ensure-signin", "start", "status", "link", "confirm-cursor"]
                );
                // Fresh evidence becomes available only after capturing both baselines
                // and presenting the wake instruction. Old evidence cannot finish setup.
                fs::write(
                    fixture.0.join("confirm-evidence"),
                    "wake=confirmed\nlink=connected\nrun=started\r\n",
                )
                .map_err(|_| Failure("output_failed"))?;
                Ok(())
            },
        )
        .await
        .map_err(diagnostic)?;
    server.await??;
    assert_eq!((prompts, instructions), (0, 1));
    assert!(
        operations(&fixture)?
            .last()
            .is_some_and(|call| call.starts_with("confirm-check '"))
    );
    no_forwards(&fixture)?;
    Ok(())
}

#[tokio::test]
async fn t3_installed_setup_reauthenticates_once_and_refuses_persistent_relink() -> Result {
    for persistent in [false, true] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let screen = prepare(&fixture, listener.local_addr()?.port())?;
        let states = if persistent {
            vec![(true, false); 2]
        } else {
            vec![(true, false), (false, false), (false, false)]
        };
        let server = tokio::spawn(health_server(listener, screen, states));
        let result = connection
            .guide(100, |_| Ok(()), || Err(Failure("instruction_barrier")))
            .await;
        assert_eq!(
            result,
            Err(Failure(if persistent {
                "setup_relink_required"
            } else {
                "instruction_barrier"
            }))
        );
        server.await??;
        let calls = operations(&fixture)?;
        assert_eq!(
            &calls[..8],
            [
                "stop",
                "ensure-signin",
                "start",
                "status",
                "stop",
                "signin",
                "start",
                "status"
            ]
        );
        assert_eq!(calls.iter().filter(|call| *call == "signin").count(), 1);
        assert_eq!(
            calls.iter().filter(|call| *call == "link").count(),
            usize::from(!persistent)
        );
        no_forwards(&fixture)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_installed_setup_stops_on_unsafe_start_kill_switch_or_stop_refusal() -> Result {
    for failure in ["identity", "kill", "stop"] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let screen = prepare(&fixture, listener.local_addr()?.port())?;
        if failure == "identity" {
            fs::write(
                fixture.0.join("status"),
                "process=running\nprocess_user=root\nuptime_seconds=1\nhassmic_link=unavailable\n",
            )?;
        } else if failure == "stop" {
            fs::write(fixture.0.join("stop-reply"), "SYNTHETIC-ACCOUNT")?;
        }
        let server = if failure == "kill" {
            Some(tokio::spawn(health_server(listener, screen, vec![(false, true)])))
        } else {
            None
        };
        let result = connection
            .guide(100, |_| Ok(()), || Err(Failure("unexpected_wake")))
            .await;
        assert_eq!(
            result,
            Err(Failure(match failure {
                "identity" => "unsafe_process_user",
                "kill" => "setup_kill_switch",
                _ => "stop_failed",
            }))
        );
        if let Some(server) = server {
            server.await??;
        }
        let calls = operations(&fixture)?;
        assert!(!calls.iter().any(|call| call == "link" || call == "confirm-cursor"));
        assert_eq!(
            calls.iter().filter(|call| *call == "ensure-signin").count(),
            usize::from(failure != "stop")
        );
        no_forwards(&fixture)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_installed_setup_one_sided_wake_evidence_expires_without_rebaselining() -> Result {
    for evidence in [
        "wake=confirmed\nlink=connected\nrun=pending\n",
        "wake=pending\nlink=pending\nrun=started\n",
    ] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let screen = prepare(&fixture, listener.local_addr()?.port())?;
        fs::write(fixture.0.join("confirm-evidence"), evidence)?;
        let server = tokio::spawn(health_server(listener, screen, vec![(false, false); 2]));
        let barrier = fixture.listener("wake-barrier")?;
        let mut instructions = 0;
        let mut guided = Box::pin(connection.guide(
            100,
            |_| Ok(()),
            || {
                instructions += 1;
                Ok(())
            },
        ));
        let mut cursor = tokio::select! {
            result = &mut guided => return Err(format!("early result: {result:?}").into()),
            socket = ready(&barrier) => socket?,
        };
        cursor.write_all(&[1]).await?;
        let child = tokio::select! {
            result = &mut guided => return Err(format!("one-sided evidence completed: {result:?}").into()),
            socket = ready(&barrier) => socket?,
        };
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::time::resume();
        assert_eq!(guided.await, Err(Failure("setup_timeout")));
        assert_eq!(instructions, 1);
        drop(child);
        server.await??;
        assert_eq!(
            operations(&fixture)?
                .iter()
                .filter(|call| *call == "confirm-cursor")
                .count(),
            1
        );
        no_forwards(&fixture)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_linked_resume_preserves_grant_and_still_requires_fresh_confirmation() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let screen = prepare(&fixture, listener.local_addr()?.port())?;
    let server =
        tokio::spawn(async move { health_settings_server(listener, screen, vec![(false, false); 2], "linked").await });
    // An existing account completes ensure-signin without a public prompt.
    // Even linked settings cannot replace the fresh wake/run requirement.
    let result = connection
        .guide(
            100,
            |_| Err(Failure("unexpected_prompt")),
            || Err(Failure("fresh_confirmation_required")),
        )
        .await;
    assert_eq!(result, Err(Failure("fresh_confirmation_required")));
    server.await??;
    assert_eq!(
        operations(&fixture)?,
        ["stop", "ensure-signin", "start", "status", "confirm-cursor"]
    );
    no_forwards(&fixture)?;
    Ok(())
}

#[tokio::test]
async fn t3_unconfigured_or_invalid_link_state_refuses_grant_mutation_and_drains_forwards() -> Result {
    for (link_state, error) in [
        ("unconfigured", "setup_link_unconfigured"),
        ("SYNTHETIC-ACCOUNT", "setup_settings_invalid"),
    ] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let screen = prepare(&fixture, listener.local_addr()?.port())?;
        let server =
            tokio::spawn(
                async move { health_settings_server(listener, screen, vec![(false, false); 2], link_state).await },
            );
        assert_eq!(
            connection
                .guide(
                    100,
                    |_| Err(Failure("unexpected_prompt")),
                    || Err(Failure("unexpected_wake"))
                )
                .await,
            Err(Failure(error))
        );
        server.await??;
        assert_eq!(operations(&fixture)?, ["stop", "ensure-signin", "start", "status"]);
        no_forwards(&fixture)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_orderly_cancellation_settles_account_before_draining_forwards() -> Result {
    for stop_failure in [false, true] {
        let fixture = Fixture::new()?;
        let connection = fixture.connection(deadline()).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let screen = prepare(&fixture, listener.local_addr()?.port())?;
        let barrier = fixture.listener("account-barrier")?;
        let cancelled = echo_setup::Cancellation::default();
        let mut guided =
            Box::pin(connection.guide_cancellable(100, |_| Ok(()), || Err(Failure("unexpected_wake")), &cancelled));
        let child = tokio::select! {
            result = &mut guided => return Err(format!("early result: {result:?}").into()),
            socket = ready(&barrier) => socket?,
        };
        if stop_failure {
            fs::write(fixture.0.join("stop-reply"), "SYNTHETIC-ACCOUNT")?;
        }
        cancelled.cancel();
        assert_eq!(
            guided.await,
            Err(Failure(if stop_failure {
                "setup_account_unsettled"
            } else {
                "setup_cancelled"
            }))
        );
        drop((child, screen, listener));
        assert_eq!(operations(&fixture)?, ["stop", "ensure-signin", "stop"]);
        no_forwards(&fixture)?;
    }
    Ok(())
}

#[tokio::test]
async fn t3_orderly_wake_cancellation_does_not_accept_complete_evidence_or_restart() -> Result {
    let fixture = Fixture::new()?;
    let connection = fixture.connection(deadline()).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let screen = prepare(&fixture, listener.local_addr()?.port())?;
    let server = tokio::spawn(health_server(listener, screen, vec![(false, false); 2]));
    let cancelled = echo_setup::Cancellation::default();
    let result = connection
        .guide_cancellable(
            100,
            |_| Ok(()),
            || {
                cancelled.cancel();
                Ok(())
            },
            &cancelled,
        )
        .await;
    assert_eq!(result, Err(Failure("setup_cancelled")));
    server.await??;
    assert_eq!(
        operations(&fixture)?,
        ["stop", "ensure-signin", "start", "status", "link", "confirm-cursor"]
    );
    no_forwards(&fixture)?;
    Ok(())
}
