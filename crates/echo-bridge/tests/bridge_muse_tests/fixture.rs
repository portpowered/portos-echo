//! T3 fixture: prebuilt Unix daemon/report/fake gadget, generated private trust.
use super::*;
use std::os::unix::fs::PermissionsExt;

pub(super) fn private(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn openssl(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("openssl")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

pub(super) fn certificate(root: &Path, name: &str, san: &str) {
    openssl(
        root,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048", // lint-rules: allow-literal OpenSSL key algorithm argument, never a network address
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=synthetic",
            "-addext",
            san,
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-addext",
            "extendedKeyUsage=serverAuth",
            "-keyout",
            &format!("{name}.pem"),
            "-out",
            &format!("{name}.crt"),
        ],
    );
    openssl(
        root,
        &[
            "x509",
            "-in",
            &format!("{name}.crt"),
            "-outform",
            "DER",
            "-out",
            &format!("{name}.der"),
        ],
    );
    openssl(
        root,
        &[
            "pkcs8",
            "-topk8",
            "-nocrypt",
            "-in",
            &format!("{name}.pem"),
            "-outform",
            "DER",
            "-out",
            &format!("{name}.key"),
        ],
    );
    for suffix in ["pem", "crt", "der", "key"] {
        std::fs::set_permissions(
            root.join(format!("{name}.{suffix}")),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}

pub(super) struct Muse {
    pub directory: tempfile::TempDir,
    pub child: Process,
    pub url: String,
}

impl Muse {
    pub async fn start(san: &str) -> Self {
        let directory = tempfile::tempdir_in(binary("echo-muse-fake-gadget").parent().unwrap()).unwrap();
        let root = directory.path();
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        certificate(root, "server", san);
        // Fresh random printable credential, never a committed fixture or logged value.
        let credential = std::fs::read(root.join("server.key")).unwrap();
        let credential: String = credential.iter().take(64).map(|b| char::from(b'a' + b % 26)).collect();
        private(&root.join("muse-credential"), credential.as_bytes());
        std::fs::hard_link(binary("echo-muse-fake-gadget"), root.join("gadget")).unwrap();
        private(
            &root.join("script.json"),
            json!({"stdout":"Done! The job completed successfully.","stderr":"","exit":0})
                .to_string()
                .as_bytes(),
        );
        let config = json!({
            "listen":"127.0.0.1:0", "tls_certificate_der":root.join("server.der"),
            "tls_private_key_der":root.join("server.key"), "client_credential_file":root.join("muse-credential"),
            "store":root.join("tasks.json"), "report_socket":root.join("report.sock"),
            "gadget_program":root.join("gadget"), "clock":{"source":"controlled_stdin","initial_unix_seconds":100}
        });
        private(&root.join("daemon.json"), config.to_string().as_bytes());
        let mut command = Command::new(binary("echo-muse-daemon"));
        command.args([
            "--config",
            root.join("daemon.json").to_str().unwrap(),
            "--control-stdin",
        ]);
        let mut child = Process::spawn(&mut command);
        let ready: Value = serde_json::from_str(&child.line().await).unwrap();
        assert_eq!(ready["ready"], true);
        let address: std::net::SocketAddr = ready["listen"].as_str().unwrap().parse().unwrap();
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
        let origin = "https://127.0.0.1";
        let url = format!("{origin}:{}", address.port());
        Self { directory, child, url }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    pub fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub async fn report(&self, id: &str) -> Value {
        let mut child = Command::new(binary("echo-muse-report"))
            .env("ECHO_MUSE_REPORT_SOCKET", self.path("report.sock"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                json!({"task_id":id,"status":"completed","summary":"Muse test job completed"})
                    .to_string()
                    .as_bytes(),
            )
            .await
            .unwrap();
        let output = tokio::time::timeout(OBSERVATION_TIMEOUT, child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(output.stderr, b"");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    pub async fn stop(mut self) {
        self.child
            .input
            .as_mut()
            .unwrap()
            .write_all(b"{\"operation\":\"shutdown\"}\n")
            .await
            .unwrap();
        self.child.finish(0).await;
        assert!(
            tokio::net::TcpStream::connect(self.url.trim_start_matches("https://"))
                .await
                .is_err()
        );
    }
}

fn binary(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_BIN_EXE_echo-bridge"))
        .parent()
        .unwrap()
        .join(name);
    assert!(
        path.is_file(),
        "prebuild required: cargo build --locked -p echo-bridge -p echo-muse-bridge --bins"
    );
    path
}

pub(super) fn configure(directory: &Path, port: u16, muse: &Muse, fault: &str) {
    bridge_wire_screen::setup(directory, port);
    let path = directory.join("bridge.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    // Omit MCP entirely: Muse owns the sole executor, no Port OS grant is mounted.
    let (head, tail) = config.split_once("[[mcp.server]]").unwrap();
    let (_, tail) = tail.split_once("[screen]").unwrap();
    let config = format!("{head}[mcp]\nserver = []\n[screen]{tail}").replace("[agent]", "[agent]\nbackend = 'muse'");
    let ca = if fault == "untrusted" {
        certificate(directory, "other", "subjectAltName=IP:127.0.0.1");
        directory.join("other.der")
    } else {
        muse.path("server.der")
    };
    let credential = if fault == "authentication" {
        private(&directory.join("wrong-credential"), &[b'x'; 64]);
        directory.join("wrong-credential")
    } else {
        muse.path("muse-credential")
    };
    let url = if fault == "unreachable" {
        "https://127.0.0.1:1"
    } else {
        &muse.url
    };
    std::fs::write(
        path,
        format!(
            "{config}\n[muse]\nurl = '{url}'\nca_file = '{}'\ncredential_file = '{}'\n",
            ca.display(),
            credential.display()
        ),
    )
    .unwrap();
}
