use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub const DAEMON: &str = env!("CARGO_BIN_EXE_echo-muse-daemon");
pub const HANDLER: &str = env!("CARGO_BIN_EXE_echo-muse-report");
pub const FAKE: &str = env!("CARGO_BIN_EXE_echo-muse-fake-gadget");
pub const CHAT: &str = "Done! The job completed successfully.";

pub fn private(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub config: Value,
    pub credential: String,
    pub client: reqwest::Client,
}

impl Fixture {
    pub fn new() -> Self {
        let _installed = rustls::crypto::ring::default_provider().install_default();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
        private(&root.join("cert.der"), generated.cert.der());
        private(&root.join("key.der"), &generated.signing_key.serialize_der());
        let mut random = [0; 32];
        getrandom::fill(&mut random).unwrap();
        let credential = random.iter().map(|b| char::from(b'a' + b % 26)).collect::<String>();
        private(&root.join("credential"), credential.as_bytes());
        std::fs::hard_link(FAKE, root.join("gadget")).unwrap();
        let config = json!({
            "listen":"127.0.0.1:0", "tls_certificate_der":root.join("cert.der"),
            "tls_private_key_der":root.join("key.der"), "client_credential_file":root.join("credential"),
            "store":root.join("tasks.json"), "report_socket":root.join("report.sock"),
            "gadget_program":root.join("gadget"),"clock":{"source":"controlled_stdin","initial_unix_seconds":100}
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_der(generated.cert.der()).unwrap())
            .timeout(Duration::from_secs(6))
            .build()
            .unwrap();
        let fixture = Self {
            temp,
            config,
            credential,
            client,
        };
        fixture.script(CHAT, "", 0, None);
        fixture
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    pub fn script(&self, stdout: &str, stderr: &str, exit: u8, gate: Option<&Path>) {
        private(
            &self.path("script.json"),
            json!({"stdout":stdout,"stderr":stderr,"exit":exit,"gate_socket":gate})
                .to_string()
                .as_bytes(),
        );
    }

    pub fn calls(&self) -> Vec<Value> {
        match std::fs::read_to_string(self.path("calls.jsonl")) {
            Ok(text) => text.lines().map(|line| serde_json::from_str(line).unwrap()).collect(),
            Err(_) => vec![],
        }
    }

    pub fn command(&self) -> Command {
        private(&self.path("config.json"), self.config.to_string().as_bytes());
        let mut cmd = Command::new(DAEMON);
        cmd.args([
            "--config",
            self.path("config.json").to_str().unwrap(),
            "--control-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
        cmd
    }

    pub async fn start(&self) -> Daemon {
        let mut child = self.command().spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["ready"], true);
        let address: std::net::SocketAddr = ready["listen"].as_str().unwrap().parse().unwrap();
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
        let mut url = reqwest::Url::parse("https://127.0.0.1").unwrap();
        url.set_port(Some(address.port())).unwrap();
        Daemon {
            child,
            stdin,
            stdout,
            url: url.as_str().trim_end_matches('/').to_string(),
        }
    }

    pub async fn request(&self, daemon: &Daemon, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", daemon.url))
            .bearer_auth(&self.credential);
        if let Some(body) = body {
            request = request.json(&body);
        }
        decode(request.send().await.unwrap()).await
    }

    pub async fn submit(&self, daemon: &Daemon) -> String {
        let (status, value) = self.request(daemon, "POST", "/v1/tasks", Some(task())).await;
        assert_eq!(status, 201, "{value}");
        value["task_id"].as_str().unwrap().to_string()
    }

    pub async fn event(&self, daemon: &Daemon, id: &str) -> (u16, Value) {
        self.request(daemon, "GET", &format!("/v1/tasks/{id}/events"), None)
            .await
    }

    pub async fn report(&self, input: &[u8]) -> std::process::Output {
        handler(&self.path("report.sock"), input).await
    }

    pub async fn accepted(&self, id: &str, status: &str, summary: &str) {
        let output = self
            .report(
                json!({"task_id":id,"status":status,"summary":summary})
                    .to_string()
                    .as_bytes(),
            )
            .await;
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(output.stderr, b"");
        assert_eq!(
            output.stdout,
            json!({"accepted":true,"task_id":id}).to_string().as_bytes()
        );
    }

    pub async fn rejected(&self, id: &str, reason: &str) {
        reject(
            &self
                .report(
                    json!({"task_id":id,"status":"completed","summary":"x"})
                        .to_string()
                        .as_bytes(),
                )
                .await,
            reason,
        );
    }
}

pub struct Daemon {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub url: String,
}

impl Daemon {
    pub async fn time(&mut self, time: u64) {
        self.stdin
            .write_all(
                json!({"operation":"set_time","unix_seconds":time})
                    .to_string()
                    .as_bytes(),
            )
            .await
            .unwrap();
        self.stdin.write_all(b"\n").await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(3), self.stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({"clock_unix_seconds":time})
        );
    }

    pub async fn stop(mut self, success: bool) {
        self.stdin.write_all(b"{\"operation\":\"shutdown\"}\n").await.unwrap();
        let output = tokio::time::timeout(Duration::from_secs(12), self.child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if success {
            assert_eq!(output.stderr, b"");
        }
    }
}

pub async fn handler(socket: &Path, input: &[u8]) -> std::process::Output {
    let mut child = Command::new(HANDLER)
        .env("ECHO_MUSE_REPORT_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).await.unwrap();
    tokio::time::timeout(Duration::from_secs(9), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

pub fn reject(output: &std::process::Output, reason: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim_end(), reason);
}

pub async fn decode(response: reqwest::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = response.bytes().await.unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

pub fn task() -> Value {
    json!({"delegation_id":"delegation-test","instructions":"Read light", "context":{"target":"test-light"},"deadline_seconds":60,"max_tool_calls":8})
}

pub fn progress(id: &str) -> (u16, Value) {
    (200, json!({"type":"progress","task_id":id,"message":CHAT}))
}
pub fn empty() -> (u16, Value) {
    (204, Value::Null)
}
