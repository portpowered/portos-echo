//! T3: paired loopback peers and fake Shell; never adb, hardware or accounts.
use super::*;
use crate::lab_run::{
    device::onboard::tests::{remote, world},
    run_report::Artifacts,
};
use std::fmt::Write;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
};

fn wire(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn request(socket: TcpStream, operation: &str) -> TcpStream {
    let mut reader = BufReader::new(socket);
    let mut headers = Vec::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.trim().parse::<usize>().unwrap();
        }
        headers.push(line);
    }
    assert_eq!(headers[0], "POST /operator/mcp HTTP/1.1\r\n");
    assert_eq!(
        headers
            .iter()
            .filter(|line| line.starts_with("Authorization:"))
            .collect::<Vec<_>>(),
        [&"Authorization: Bearer synthetic-local-screen\r\n".to_owned()]
    );
    assert!(!headers.iter().any(|line| line.starts_with("Origin:")));
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"operation":operation})
    );
    reader.into_inner()
}

#[test]
fn mcp_device_operator_response_requires_exact_status_framing_and_outcome() {
    assert_eq!(
        response(wire("200 OK", r#"{"outcome":"linked"}"#).as_bytes(), "linked"),
        Ok(())
    );
    for body in [
        r#"{"outcome":"refreshed"}"#,
        r#"{"outcome":"linked","extra":1}"#,
        "null",
        "bad",
    ] {
        assert_eq!(response(wire("200 OK", body).as_bytes(), "linked"), Err(CONTROL.into()));
    }
    let good = wire("200 OK", r#"{"outcome":"linked"}"#);
    for bad in [
        good.replace("200 OK", "302 Found"),
        good.replace("Content-Length: 20", "Content-Length: 0"),
        good.replace("Content-Type:", "Transfer-Encoding: chunked\r\nContent-Type:"),
        good.replace("Content-Type:", "Content-Length: 20\r\nContent-Type:"),
        good.replace("application/json", "text/plain"),
        format!("{good}trailing"),
        "x".repeat(8193),
    ] {
        assert_eq!(response(bad.as_bytes(), "linked"), Err(CONTROL.into()));
    }
}

#[test]
fn mcp_device_operator_maps_only_known_error_categories_without_provider_text() {
    for category in ["access_denied", "expired", "relink_required", "storage"] {
        assert_eq!(
            response(
                wire("422 Unprocessable Entity", &json!({"error":category}).to_string()).as_bytes(),
                "linked"
            ),
            Err(format!("device: MCP operator {category}"))
        );
    }
    assert_eq!(
        response(
            wire("422 Unprocessable Entity", r#"{"error":"private-provider-message"}"#).as_bytes(),
            "linked"
        ),
        Err(CONTROL.into())
    );
}

fn configured(world: &std::rc::Rc<std::cell::RefCell<super::super::tests::World>>) -> (Runtime, String) {
    let config = include_str!("../../../../../../examples/bridge.echo.toml").to_owned();
    let mut state = Runtime::default();
    state.configure_portos(&config).unwrap();
    state.remote = Some(remote(world));
    state.generation = 1;
    state.started = true;
    state.began = Some(std::time::Instant::now());
    world.borrow_mut().trace = format!(
        "{}\n",
        json!({"event":"mcp.operator.listening","elapsed_ms":1,"address":"127.0.0.1:16902"})
    );
    (state, config)
}

#[tokio::test]
async fn t3_on_device_mcp_operator_uses_owned_forward_pairing_and_exact_completions() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let world = world();
    world.borrow_mut().forward = listener.local_addr().unwrap().port();
    let (mut state, config) = configured(&world);
    let peer = tokio::spawn(async move {
        for (operation, outcome) in [("link", "linked"), ("refresh", "refreshed"), ("unlink", "unlinked")] {
            let socket = listener.accept().await.unwrap().0;
            let mut socket = request(socket, operation).await;
            socket
                .write_all(wire("200 OK", &json!({"outcome":outcome}).to_string()).as_bytes())
                .await
                .unwrap();
        }
    });
    assert_eq!(
        state.mcp_operator(&McpAction::Approve, &config).await,
        Err("device: no pending MCP link".into())
    );
    state.mcp_operator(&McpAction::Link, &config).await.unwrap();
    assert!(state.pending_mcp.is_some());
    assert_eq!(
        state.mcp_operator(&McpAction::Refresh, &config).await,
        Err("device: MCP link already pending".into())
    );
    state.mcp_operator(&McpAction::Approve, &config).await.unwrap();
    assert!(state.pending_mcp.is_none());
    state.mcp_operator(&McpAction::Refresh, &config).await.unwrap();
    state.mcp_operator(&McpAction::Unlink, &config).await.unwrap();
    peer.await.unwrap();
    assert_eq!(
        state.portos.as_ref().unwrap().measure().unwrap(),
        echo_lab::lab_mcp::LinkCounts::default()
    );
    let mut remote = state.remote.take().unwrap();
    remote.release().unwrap();
    let world = world.borrow();
    assert_eq!(
        world
            .calls
            .iter()
            .filter(|args| args[0] == "forward" && args[1] == "tcp:0") // lint-rules: allow-literal synthetic adb comparison
            .count(),
        1
    );
    assert!(world.ran("head -c 16385 /data/local/echo-bridge/private/screen-pairing.txt"));
    assert!(world.ran(&format!("forward --remove tcp:{}", world.forward)));
}

#[tokio::test]
async fn t3_on_device_mcp_operator_cancel_closes_pending_link_before_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let world = world();
    world.borrow_mut().forward = listener.local_addr().unwrap().port();
    let (mut state, config) = configured(&world);
    let (sent, received) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut socket = request(listener.accept().await.unwrap().0, "link").await;
        sent.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
    });
    state.mcp_operator(&McpAction::Link, &config).await.unwrap();
    received.await.unwrap();
    let cancel = root.path().join("cancel");
    std::fs::write(&cancel, "cancel").unwrap();
    assert_eq!(
        crate::lab_run::device::runtime::controlled(state.mcp_operator(&McpAction::Approve, &config), Some(&cancel))
            .await,
        Err("run cancelled".into())
    );
    let mut artifacts = Artifacts::create(root.path().join("out")).unwrap();
    state.close(root.path(), &mut artifacts).await.unwrap();
    assert!(state.pending_mcp.is_none());
    peer.await.unwrap();
}

#[tokio::test]
async fn t3_on_device_mcp_operator_denial_clears_pending_without_approval() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let world = world();
    world.borrow_mut().forward = listener.local_addr().unwrap().port();
    let (mut state, config) = configured(&world);
    let peer = tokio::spawn(async move {
        let mut socket = request(listener.accept().await.unwrap().0, "link").await;
        socket
            .write_all(wire("422 Unprocessable Entity", r#"{"error":"access_denied"}"#).as_bytes())
            .await
            .unwrap();
    });
    state.mcp_operator(&McpAction::Link, &config).await.unwrap();
    assert_eq!(
        state.mcp_operator(&McpAction::Approve, &config).await,
        Err("device: MCP operator access_denied".into())
    );
    assert!(state.pending_mcp.is_none());
    assert_eq!(state.portos.as_ref().unwrap().measure().unwrap().link, 0);
    peer.await.unwrap();
}

#[tokio::test]
async fn t3_on_device_mcp_operator_rejects_missing_ambiguous_or_nonlocal_listener_before_forward() {
    for records in [
        Vec::new(),
        vec!["127.0.0.1:16902", "127.0.0.1:16902"],
        vec!["0.0.0.0:16902"], // lint-rules: allow-literal refused listener, never connected
        vec!["127.0.0.1:0"],
        vec!["localhost:16902"],
    ] {
        let world = world();
        let (mut state, config) = configured(&world);
        world.borrow_mut().trace.clear();
        for address in records {
            writeln!(
                world.borrow_mut().trace,
                "{}",
                json!({"event":"mcp.operator.listening","elapsed_ms":1,"address":address})
            )
            .unwrap();
        }
        assert_eq!(state.mcp_operator(&McpAction::Link, &config).await, Err(CONTROL.into()));
        assert!(state.pending_mcp.is_none());
        assert!(!world.borrow().calls.iter().any(|args| args[0] == "forward"));
    }
}

struct TokenShell(Box<dyn super::super::Shell>, Vec<u8>);

impl super::super::Shell for TokenShell {
    fn adb(&self, args: &[String]) -> Result<Vec<u8>, String> {
        if args.iter().any(|arg| arg.contains("head -c 16385")) {
            return Ok(self.1.clone());
        }
        self.0.adb(args)
    }
}

#[tokio::test]
async fn t3_on_device_mcp_operator_pairing_refuses_empty_controls_oversize_and_shell_paths() {
    for bytes in [
        vec![],
        b" \n".to_vec(),
        b"secret\r\ninjected".to_vec(),
        vec![b'x'; 16385],
        vec![255],
    ] {
        let world = world();
        let (mut state, config) = configured(&world);
        let mut remote = state.remote.take().unwrap();
        remote.shell = Box::new(TokenShell(remote.shell, bytes));
        state.remote = Some(remote);
        assert_eq!(state.mcp_operator(&McpAction::Link, &config).await, Err(PAIRING.into()));
        assert!(!world.borrow().calls.iter().any(|args| args[0] == "forward"));
    }
    let world = world();
    let (mut state, config) = configured(&world);
    let config = config.replace("screen-pairing.txt", "pair;command");
    assert_eq!(state.mcp_operator(&McpAction::Link, &config).await, Err(PAIRING.into()));
    assert!(!world.borrow().ran("head -c"));
}
