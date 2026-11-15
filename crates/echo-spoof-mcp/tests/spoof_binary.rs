//! T3: runs the prebuilt executable; no compilation or external service inside tests.
#![cfg(test)]
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(args: &[&str]) -> Guard {
    Guard(
        Command::new(env!("CARGO_BIN_EXE_echo-spoof-mcp"))
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}
async fn line(child: &mut Guard) -> String {
    let stdout = child.0.stdout.take().unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        tokio::task::spawn_blocking(move || {
            let mut line = String::new();
            BufReader::new(stdout).read_line(&mut line).unwrap();
            line
        }),
    )
    .await
    .unwrap()
    .unwrap()
}
fn request(id: u64, method: &str, params: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
async fn post(url: &str, body: &Value) -> reqwest::Response {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
        .post(url)
        .json(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn t3_f18_binary_readiness_catalog_get_faults_and_reaping() {
    for flags in [
        vec![],
        vec!["--fault", "http_500"],
        vec!["--fault", "malformed_json_rpc"],
        vec!["--fault", "slow", "--slow-ms", "0"],
    ] {
        let mut child = spawn(&flags);
        let ready: Value = serde_json::from_str(&line(&mut child).await).unwrap();
        assert_eq!(ready["event"], "ready");
        let url = ready["url"].as_str().unwrap();
        let parsed = reqwest::Url::parse(url).unwrap();
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_ne!(parsed.port(), Some(0));
        let catalog = post(url, &request(1, "tools/list", &json!({})))
            .await
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(
            catalog,
            json!({"jsonrpc":"2.0","id":1,"result":{"tools":echo_spoof_mcp::catalog()}})
        );
        let get = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        assert_eq!(get.status(), 405);
        let call = post(
            url,
            &request(
                2,
                "tools/call",
                &json!({"name":"test.echo","arguments":{"text":"hello"}}),
            ),
        )
        .await;
        if flags.contains(&"http_500") {
            assert_eq!(call.status(), 500);
        } else if flags.contains(&"malformed_json_rpc") {
            assert_eq!(call.json::<Value>().await.unwrap(), json!({"invalid":true}));
        } else {
            assert_eq!(
                call.json::<Value>().await.unwrap(),
                json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"hello"}],"isError":false}})
            );
        }
        reap(&mut child, &flags, &catalog);
    }
}

#[tokio::test]
async fn t3_f18_invalid_flags_and_busy_port_exit_without_ready() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    for args in [
        vec!["--port", port.as_str()],
        vec!["--bad"],
        vec!["--port", "65536"],
        vec!["--fault", "unknown"],
    ] {
        let mut child = spawn(&args);
        assert_eq!(line(&mut child).await, "");
        assert_eq!(child.0.wait().unwrap().code(), Some(1));
        let mut stderr = String::new();
        child.0.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
        assert_eq!(stderr, "echo-spoof-mcp: invalid arguments or server failure\n");
    }
}

#[tokio::test]
async fn t3_f18_help_is_exact_and_opens_no_listener() {
    let mut child = spawn(&["--help"]);
    let stdout = child.0.stdout.take().unwrap();
    let text = tokio::task::spawn_blocking(move || {
        let mut text = String::new();
        BufReader::new(stdout).read_to_string(&mut text).unwrap();
        text
    })
    .await
    .unwrap();
    assert_eq!(child.0.wait().unwrap().code(), Some(0));
    assert_eq!(
        text,
        "echo-spoof-mcp [--port <0..65535>] [--fault <http_500|slow|malformed_json_rpc>] [--slow-ms <unsigned-ms>]\necho-spoof-mcp --help\n"
    );
}

fn reap(child: &mut Guard, flags: &[&str], catalog: &Value) {
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let remaining = usize::from(child.0.try_wait().unwrap().is_none());
    assert_eq!(remaining, 0);
    if flags.is_empty() {
        writeln!(
            std::io::stdout(),
            "ready_host=127.0.0.1 tools={} get_status=405 remaining_children={remaining}",
            catalog["result"]["tools"].as_array().unwrap().len()
        )
        .unwrap();
    }
}
