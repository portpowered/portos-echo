use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{io::Write, os::unix::fs::PermissionsExt, process::Stdio};
use tokio::{io::AsyncBufReadExt, process::Command};

const PEER: &str = r"
import http.server, json, ssl, sys
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        assert self.path == '/v1/tasks'
        assert self.headers.get('Content-Length', '0') == '0'
        assert self.headers.get('DPoP') is None
        accepted = self.headers.get('Authorization') == 'Bearer ' + 'x' * 32
        status, code = (405, 'method_not_allowed') if accepted else (401, 'unauthorized')
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps({'error': code, 'fixture': 'private-remote-detail'}).encode())
        print(status, flush=True)
server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
tls.load_cert_chain('cert.pem', 'key.pem')
server.socket = tls.wrap_socket(server.socket, server_side=True)
print(server.server_address[1], flush=True)
for _ in range(2):
    server.handle_request()
";

#[tokio::test]
async fn t3_settings_muse_save_probe_rotate_and_reload_private_connection() {
    let (mut fixture, owner, _) = Fixture::open("[agent]\ndaily_request_cap=8\n").await;
    let base_path = fixture.root.path().join("bridge.toml");
    let base = fs::read(&base_path).unwrap();
    let (origin, mut peer, mut receipts, der) = peer(fixture.root.path()).await;
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let trace = fixture.root.path().join("trace");
    let owner = owner.with_trace(crate::bridge_trace::Trace::open(&trace, "chatgpt").unwrap());
    let journey = async {
        save_and_test(&fixture, &origin, &der).await;
        assert_eq!(receipts.next_line().await.unwrap().as_deref(), Some("405"));
        assert_eq!(receipts.next_line().await.unwrap().as_deref(), Some("401"));
        assert!(peer.wait().await.unwrap().success());
        probe(&fixture, json!({"version":2,"reachable":false,"reason":"unavailable"})).await;
        let effective = BridgeConfig::load_effective(&base_path).unwrap();
        assert!(effective.warning.is_none());
        assert_eq!(effective.config.agent.daily_request_cap, 8);
        let muse = effective.config.muse.unwrap();
        assert_eq!(muse.url, origin);
        for (path, bytes) in [(&muse.ca_file, der.as_slice()), (&muse.credential_file, &[b'y'; 32])] {
            assert!(path.starts_with(fixture.root.path().join("home/muse")));
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert_eq!(fs::read(&base_path).unwrap(), base);
        assert!(!fixture.overlay().contains(&"x".repeat(32)));
        assert!(!fixture.overlay().contains(&"y".repeat(32)));
        let text = fs::read_to_string(&trace).unwrap();
        assert!(!text.contains(&STANDARD.encode(&der)));
        assert!(!text.contains("private-remote-detail"));
        assert!(!text.contains(&"x".repeat(32)));
        assert!(!text.contains(&"y".repeat(32)));
        stop.send(()).unwrap();
    };
    let (((), completion), ()) = tokio::join!(
        owner.scope(async { stopping.await.unwrap() }, || LinkStatusV1::Unconfigured),
        journey
    );
    assert!(completion.restart_requested);
    fixture.server.shutdown().await.unwrap();
    writeln!(
        std::io::stdout().lock(),
        "Muse probe GET receipts=405,401; down=unavailable; mode=0600; durable reload; base and sibling retained"
    )
    .unwrap();
}

async fn save_and_test(fixture: &Fixture, origin: &str, der: &[u8]) {
    assert_unconfigured(fixture).await;
    let accepted = save_connection(fixture, origin, der).await;
    refuse_changed_origin(fixture, &accepted).await;
    guard_probes(fixture).await;
    rotate_credential(fixture, &accepted["muse"]).await;
}

async fn assert_unconfigured(fixture: &Fixture) {
    let old = snapshot(fixture, false).await;
    assert!(old.get("muse").is_none());
    let initial = snapshot(fixture, true).await;
    assert_eq!(initial["muse"], json!({"configured":false,"reachability":"unchecked"}));
    probe(fixture, json!({"version":2,"reachable":false,"reason":"unconfigured"})).await;
}

async fn save_connection(fixture: &Fixture, origin: &str, der: &[u8]) -> Value {
    let saved = patch(
        fixture,
        json!({"url":origin,"ca":STANDARD.encode(der),"credential":"x".repeat(32)}),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(
        saved.json::<Value>().await.unwrap(),
        json!({"version":2,"restart_pending":true})
    );
    let accepted = snapshot(fixture, true).await;
    assert_eq!(
        accepted["muse"],
        json!({"configured":true,"url":origin,"reachability":"unchecked"})
    );
    assert!(snapshot(fixture, false).await.get("muse").is_none());
    accepted
}

async fn rotate_credential(fixture: &Fixture, unchecked: &Value) {
    probe(fixture, json!({"version":2,"reachable":true})).await;
    assert_eq!(snapshot(fixture, true).await["muse"]["reachability"], "reachable");
    let rotated = patch(fixture, json!({"credential":"y".repeat(32)})).await;
    assert_eq!(rotated.status(), StatusCode::OK);
    assert_eq!(&snapshot(fixture, true).await["muse"], unchecked);
    probe(fixture, json!({"version":2,"reachable":false,"reason":"unauthorized"})).await;
}

async fn refuse_changed_origin(fixture: &Fixture, accepted: &Value) {
    let bad = patch(
        fixture,
        json!({"url":"https://127.0.0.1:1","credential":"private-refused"}),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        bad.json::<Value>().await.unwrap(),
        json!({"version":2,"message":"muse.ca: expected a valid DER certificate"})
    );
    assert_eq!(&snapshot(fixture, true).await, accepted);
}

async fn patch(fixture: &Fixture, muse: Value) -> reqwest::Response {
    fixture
        .client
        .patch(fixture.url("/settings/v2"))
        .json(&json!({
            "version":2,"settings":{"muse":muse}
        }))
        .send()
        .await
        .unwrap()
}

async fn guard_probes(fixture: &Fixture) {
    let route = fixture.url("/settings/v2/muse");
    let valid = json!({"version":2,"operation":"probe"});
    for (request, status) in [
        (
            fixture.client.post(route.replace(TOKEN, "wrong")).body("invalid"),
            StatusCode::UNAUTHORIZED,
        ),
        (
            fixture
                .client
                .post(&route)
                .header("origin", "http://127.0.0.1:1")
                .json(&valid),
            StatusCode::FORBIDDEN,
        ),
        (
            fixture.client.post(&route).body("{}"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            fixture
                .client
                .post(&route)
                .json(&json!({"version":1,"operation":"probe"})),
            StatusCode::BAD_REQUEST,
        ),
        (
            fixture
                .client
                .post(&route)
                .json(&json!({"version":2,"operation":"probe","extra":"x".repeat(1024)})),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let refused = request.send().await.unwrap();
        assert_eq!(refused.status(), status);
        assert_eq!(refused.bytes().await.unwrap().as_ref(), b"");
    }
}

async fn snapshot(fixture: &Fixture, muse: bool) -> Value {
    let response = fixture
        .client
        .get(fixture.url("/settings/v2"))
        .query(&[("include_muse", muse)])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    response.json().await.unwrap()
}

async fn probe(fixture: &Fixture, expected: Value) {
    let response = fixture
        .client
        .post(fixture.url("/settings/v2/muse"))
        .json(&json!({"version":2,"operation":"probe"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.json::<Value>().await.unwrap(), expected);
}

async fn peer(
    root: &std::path::Path,
) -> (
    String,
    tokio::process::Child,
    tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    Vec<u8>,
) {
    let cert = Command::new("openssl")
        .current_dir(root)
        .args([
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=synthetic",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-keyout",
            "key.pem",
            "-out",
            "cert.pem",
        ])
        .output()
        .await
        .unwrap();
    assert!(cert.status.success());
    let der = Command::new("openssl")
        .current_dir(root)
        .args(["x509", "-in", "cert.pem", "-outform", "DER"])
        .output()
        .await
        .unwrap();
    assert!(der.status.success());
    let mut child = Command::new("python3")
        .args(["-u", "-c", PEER])
        .current_dir(root)
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let port: u16 = output.next_line().await.unwrap().unwrap().parse().unwrap();
    (format!("https://127.0.0.1:{port}"), child, output, der.stdout)
}
