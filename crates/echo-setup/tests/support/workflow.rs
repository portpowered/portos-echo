//! T3 installed setup sequence with real child processes and local health HTTP.
//! Provider prompts are protected by guided.rs; this protects their sequencing
//! with lifecycle, health admission and paired fresh wake/run observations.
use super::*;

fn prepare(fixture: &Fixture, health: u16) -> Result {
    fs::write(fixture.0.join("stop-reply"), "echo-install: stopped\r\n")?;
    for verb in ["ensure-signin", "signin", "link"] {
        fs::write(
            fixture.0.join(format!("{verb}-reply")),
            format!("echo-install: {verb} completed\r\n"),
        )?;
    }
    fs::write(fixture.0.join("setup-pairing"), "SYNTHETIC-PAIR\n")?;
    let replies: Vec<_> = (0..8).flat_map(|_| ["41001".to_owned(), health.to_string()]).collect();
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
    Ok(())
}

async fn health_server(listener: TcpListener, states: Vec<(bool, bool)>) -> Result {
    for (relink, kill) in states {
        let (mut socket, _) = listener.accept().await?;
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await?;
            request.push(byte[0]);
        }
        assert_eq!(
            String::from_utf8(request)?.lines().next(),
            Some("GET /healthz HTTP/1.1")
        );
        let body = serde_json::json!({"status":"running", "wyoming":"connected", "live":"idle", "relink_required":relink, "kill_switch":kill}).to_string();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
    }
    Ok(())
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
    prepare(&fixture, listener.local_addr()?.port())?;
    let server = tokio::spawn(health_server(listener, vec![(false, false); 2]));
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
        prepare(&fixture, listener.local_addr()?.port())?;
        let states = if persistent {
            vec![(true, false); 2]
        } else {
            vec![(true, false), (false, false), (false, false)]
        };
        let server = tokio::spawn(health_server(listener, states));
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
        prepare(&fixture, listener.local_addr()?.port())?;
        if failure == "identity" {
            fs::write(
                fixture.0.join("status"),
                "process=running\nprocess_user=root\nuptime_seconds=1\nhassmic_link=unavailable\n",
            )?;
        } else if failure == "stop" {
            fs::write(fixture.0.join("stop-reply"), "SYNTHETIC-ACCOUNT")?;
        }
        let server = if failure == "kill" {
            Some(tokio::spawn(health_server(listener, vec![(false, true)])))
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
        prepare(&fixture, listener.local_addr()?.port())?;
        fs::write(fixture.0.join("confirm-evidence"), evidence)?;
        let server = tokio::spawn(health_server(listener, vec![(false, false); 2]));
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
