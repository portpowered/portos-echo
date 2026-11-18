//! T3: real operator/screen/supervisor with a controlled loopback account child.
use super::*;
use crate::bridge_operator::parse;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
// Observation ceiling; no product timeout changes.
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);

#[path = "../../tests/codex_operator/fixture.rs"]
mod fixture;
use fixture::Fixture;

mod bridge_wire_auth {
    use super::*;

    pub fn config(directory: &Path, mode: &str, auth: &str) {
        let template = include_str!("../../../../examples/bridge.example.toml")
            .replace("echo.example.invalid", "127.0.0.1")
            .replace("0.0.0.0:16900", "127.0.0.1:0") // lint-rules: allow-literal replace inert example listener with loopback
            .replace("credentials/live.json", "credential")
            .replace("mode = \"chatgpt\"", &format!("mode = \"{mode}\""));
        std::fs::write(directory.join("bridge.toml"), format!("{template}\n{auth}")).unwrap();
    }
}

fn prepare(fixture: &Fixture) -> Prepared {
    parse(
        &[
            "signin".into(),
            "--config".into(),
            fixture.directory.path().join("bridge.toml").display().to_string(),
        ],
        false,
    )
    .unwrap()
    .unwrap()
    .prepare()
    .unwrap()
}

async fn released(fixture: &Fixture, actions: &mut tokio::sync::mpsc::Receiver<echo_screen_contract::UiAction>) {
    drop(fixture.owner());
    tokio::time::timeout(OBSERVATION_TIMEOUT, fixture.children_released())
        .await
        .unwrap();
    // All action senders belong to this listener's router and accepted tasks.
    // Their destruction follows the explicit listener drop in screen_listener.
    assert_eq!(
        actions.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
    );
    assert!(!fixture.home.join("auth.json").exists());
}

#[tokio::test]
async fn codex_operator_cancellation_and_output_failure_await_cleanup() {
    let fixture = Fixture::new("chatgpt");
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let (observe, observed) = tokio::sync::oneshot::channel();
    let operation = async {
        let mut output = Vec::new();
        let result = execute(
            prepare(&fixture),
            &mut output,
            async {
                let _ = cancelled.await;
            },
            Some(observe),
        )
        .await;
        (result, output)
    };
    let monitor = async {
        let mut actions = observed.await.unwrap();
        assert_eq!(actions.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty));
        let control = fixture.control().await;
        let mut socket = tokio::net::TcpStream::connect(fixture.screen).await.unwrap();
        socket
            .write_all(b"GET /events?token=synthetic-screen-pair HTTP/1.1\r\nHost: local\r\n\r\n")
            .await
            .unwrap();
        let mut screen = BufReader::new(socket).lines();
        assert_eq!(screen.next_line().await.unwrap().unwrap(), "HTTP/1.1 200 OK");
        loop {
            let line = screen.next_line().await.unwrap().unwrap();
            if let Some(data) = line.strip_prefix("data: ") {
                let state: serde_json::Value = serde_json::from_str(data).unwrap();
                if state["state"]["message"] == "code: TEST-CODE\nurl: http://127.0.0.1:12345/verify\n" {
                    break;
                }
            }
        }
        assert_eq!(actions.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty));
        cancel.send(()).unwrap();
        drop(control);
        (actions, screen)
    };
    let ((result, output), (mut actions, mut screen)) = tokio::join!(operation, monitor);
    assert_eq!(result, Err(Error::Control));
    assert_eq!(output, b"code: TEST-CODE\nurl: http://127.0.0.1:12345/verify\n");
    released(&fixture, &mut actions).await;
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        while let Some(line) = screen.next_line().await.unwrap() {
            assert!(!line.starts_with("data: "));
        }
    })
    .await
    .unwrap();
    output_failure(&fixture).await;
}

async fn output_failure(fixture: &Fixture) {
    let (observe, observed) = tokio::sync::oneshot::channel();
    let operation = async {
        let mut writer = std::io::Cursor::new([0_u8; 0]);
        execute(prepare(fixture), &mut writer, std::future::pending(), Some(observe)).await
    };
    let monitor = async {
        let mut actions = observed.await.unwrap();
        assert_eq!(actions.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty));
        actions
    };
    let (result, mut actions) = tokio::join!(operation, monitor);
    assert_eq!(result, Err(Error::Control));
    released(fixture, &mut actions).await;
}
