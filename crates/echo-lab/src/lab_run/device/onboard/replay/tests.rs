//! T1 snapshot/proof rules and T3 local HTTPS actor; no hardware or accounts.
use super::*;
use p256::ecdsa::signature::Verifier;
use tokio::io::{AsyncBufReadExt, BufReader};

const CONFIG: &str = include_str!("../../../../../../../examples/bridge.echo.toml");

fn record() -> Value {
    let key = SigningKey::from_slice(&[7; 32]).unwrap();
    let public = key.verifying_key().to_sec1_point(false);
    let config = echo_config::BridgeConfig::from_toml_str(CONFIG).unwrap();
    json!({"version":1,"issuer":"https://127.0.0.1:1","resource":config.mcp.server[0].url,
        "client_id":config.mcp.client_id,"private_key_jwk":{"kty":"EC","crv":"P-256",
        "x":B64.encode(&public.as_bytes()[1..33]),"y":B64.encode(&public.as_bytes()[33..65]),"d":B64.encode([7;32])},
        "grant":{"refresh_token":"synthetic-consumed","access_token":"synthetic-access"}})
}

fn bytes(value: &Value) -> Secret<Vec<u8>> {
    Secret::new(serde_json::to_vec(value).unwrap())
}

#[test]
fn mcp_device_replay_requires_rotated_token_same_key_and_config_identity() {
    let old = record();
    let mut new = old.clone();
    new["grant"]["refresh_token"] = "synthetic-current".into();
    assert_eq!(rotated(&bytes(&old), &bytes(&new), CONFIG).unwrap().expose(), &old);
    for key in ["issuer", "resource", "client_id", "private_key_jwk"] {
        let mut bad = new.clone();
        bad[key] = Value::Null;
        assert!(matches!(rotated(&bytes(&old), &bytes(&bad), CONFIG), Err(e) if e == ERROR));
    }
    for token in [Value::Null, json!(""), old["grant"]["refresh_token"].clone()] {
        new["grant"]["refresh_token"] = token;
        assert!(matches!(rotated(&bytes(&old), &bytes(&new), CONFIG), Err(e) if e == ERROR));
    }
    for issuer in ["http://127.0.0.1:1", "https://user@127.0.0.1", "https://127.0.0.1/#bad"] {
        assert_eq!(https(issuer), Err(ERROR.into()));
    }
}

#[test]
fn mcp_device_replay_proof_is_signed_by_snapshot_key_with_unique_fresh_claims() {
    let record = record();
    let endpoint = "https://127.0.0.1/token";
    let first = proof(&record, endpoint).unwrap();
    let second = proof(&record, endpoint).unwrap();
    let claims = |token: &str| {
        let parts: Vec<_> = token.split('.').collect();
        let key = SigningKey::from_slice(&[7; 32]).unwrap();
        let signature = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
        key.verifying_key()
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
        let header: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["typ"], "dpop+jwt");
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["jwk"].as_object().unwrap().len(), 4);
        serde_json::from_slice::<Value>(&B64.decode(parts[1]).unwrap()).unwrap()
    };
    let a = claims(first.expose());
    let b = claims(second.expose());
    assert_eq!(a["htu"], endpoint);
    assert_eq!(a["htm"], "POST");
    assert_ne!(a["jti"], b["jti"]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now.abs_diff(a["iat"].as_u64().unwrap()) < 2);
    let mut bad = record;
    bad["private_key_jwk"]["x"] = "wrong".into();
    assert!(matches!(proof(&bad, endpoint), Err(e) if e == ERROR));
}

struct Peer {
    child: tokio::process::Child,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    root: tempfile::TempDir,
    origin: String,
}

impl Peer {
    async fn start(mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut child = tokio::process::Command::new("python3")
            .args(["-I", "-c", include_str!("peer.py")])
            .arg(root.path())
            .arg(mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let origin = lines.next_line().await.unwrap().unwrap();
        Self {
            child,
            lines,
            root,
            origin,
        }
    }

    fn pem(&self) -> Vec<u8> {
        std::fs::read(self.root.path().join("cert.pem")).unwrap()
    }

    async fn finish(mut self) -> Value {
        let value = self.lines.next_line().await.unwrap().unwrap();
        assert!(self.child.wait().await.unwrap().success());
        serde_json::from_str(&value).unwrap()
    }
}

#[tokio::test]
async fn t3_on_device_mcp_replay_observes_tls_invalid_grant_with_old_token_and_same_key() {
    let peer = Peer::start("reject").await;
    let mut record = record();
    record["issuer"] = peer.origin.clone().into();
    reject("python3", &Secret::new(record.clone()), &peer.pem())
        .await
        .unwrap();
    let report = peer.finish().await;
    assert_eq!(report["requests"], 2);
    assert_eq!(report["discovery_credentials"], false);
    assert_eq!(
        report["form"],
        json!({"grant_type":["refresh_token"],
        "client_id":[record["client_id"]],"refresh_token":["synthetic-consumed"]})
    );
    let token = report["proof"].as_str().unwrap();
    let parts: Vec<_> = token.split('.').collect();
    let signature = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
    SigningKey::from_slice(&[7; 32])
        .unwrap()
        .verifying_key()
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();
    let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["htu"], format!("{}/token", record["issuer"].as_str().unwrap()));
}

#[tokio::test]
async fn t3_on_device_mcp_replay_refuses_discovery_redirects_and_false_rejections() {
    for mode in [
        "wrong-issuer",
        "foreign-token",
        "redirect",
        "success",
        "wrong-error",
        "oversize",
    ] {
        let peer = Peer::start(mode).await;
        let mut record = record();
        record["issuer"] = peer.origin.clone().into();
        assert_eq!(
            reject("python3", &Secret::new(record), &peer.pem()).await,
            Err(ERROR.into())
        );
        let report = peer.finish().await;
        assert_eq!(
            report["requests"],
            if ["success", "wrong-error"].contains(&mode) {
                2
            } else {
                1
            }
        );
        assert_eq!(report["discovery_credentials"], false);
    }
}

#[tokio::test]
async fn t3_on_device_mcp_replay_refuses_untrusted_or_malformed_pem_without_credentials() {
    for pem in [Vec::new(), b"PRIVATE-PEM-MARKER".to_vec()] {
        let mut peer = Peer::start("reject").await;
        let mut record = record();
        record["issuer"] = peer.origin.clone().into();
        assert_eq!(reject("python3", &Secret::new(record), &pem).await, Err(ERROR.into()));
        peer.child.kill().await.unwrap();
    }
    let mut peer = Peer::start("reject").await;
    let unrelated = Peer::start("reject").await;
    let mut record = record();
    record["issuer"] = peer.origin.clone().into();
    assert_eq!(
        reject("python3", &Secret::new(record), &unrelated.pem()).await,
        Err(ERROR.into())
    );
    peer.child.kill().await.unwrap();
}

#[tokio::test]
async fn t3_on_device_mcp_replay_cancel_closes_tls_child_without_token_request() {
    let mut peer = Peer::start("hang").await;
    let mut record = record();
    record["issuer"] = peer.origin.clone().into();
    let record = Secret::new(record);
    let pem = peer.pem();
    let mut pending = Box::pin(reject("python3", &record, &pem));
    tokio::select! {
        line = peer.lines.next_line() => assert_eq!(line.unwrap().as_deref(), Some("pending")),
        result = &mut pending => assert_eq!(result, Err("unexpected completion".into())),
    }
    drop(pending);
    let report = peer.finish().await;
    assert_eq!(report["closed"], true, "peer_error={}", report["peer_error"]);
    assert_eq!(report["requests"], 1);
    assert_eq!(report["discovery_credentials"], false);
}

async fn cancelled_report(mode: &str) -> Value {
    let mut peer = Peer::start(mode).await;
    let mut record = record();
    record["issuer"] = peer.origin.clone().into();
    let record = Secret::new(record);
    let pem = peer.pem();
    let mut pending = Box::pin(reject("python3", &record, &pem));
    tokio::select! {
        line = peer.lines.next_line() => assert_eq!(line.unwrap().as_deref(), Some("pending")),
        result = &mut pending => assert_eq!(result, Err("unexpected completion".into())),
    }
    drop(pending);
    peer.finish().await
}

#[tokio::test]
async fn t3_on_device_mcp_replay_write_after_close_counts_as_closed() {
    for (mode, error) in [
        ("hang-write-after-close", "BrokenPipeError"),
        ("hang-reset-after-close", "ConnectionResetError"),
    ] {
        let report = cancelled_report(mode).await;
        assert_eq!(report["closed"], true);
        assert_eq!(report["requests"], 1);
        assert_eq!(report["discovery_credentials"], false);
        assert_eq!(report["write_attempted"], true);
        assert_eq!(report["write_error"], error);
        assert_eq!(report.get("peer_error"), None);
        println!("{mode}: {report}");
    }
}

#[tokio::test]
async fn t3_on_device_mcp_replay_peer_keeps_unexpected_errors_and_data_visible() {
    let report = cancelled_report("hang-unexpected-after-close").await;
    assert_ne!(report["closed"], true);
    assert_eq!(report["requests"], 1);
    assert_eq!(report["discovery_credentials"], false);
    assert_eq!(report["write_attempted"], true);
    assert_eq!(report["write_error"], "ValueError");
    assert_eq!(
        report["peer_error"],
        json!({"type":"ValueError","errno":null,"reason":null})
    );
    println!("unexpected writer: {report}");

    let mut peer = Peer::start("hang-write-after-close").await;
    let client = tokio::process::Command::new("python3")
        .args([
            "-I",
            "-c",
            r#"
import http.client, ssl, sys
from urllib.parse import urlsplit
origin = urlsplit(sys.argv[1])
context = ssl.create_default_context(cafile=sys.argv[2])
client = http.client.HTTPSConnection(origin.hostname, origin.port, context=context)
client.putrequest("GET", "/.well-known/oauth-authorization-server")
client.endheaders()
client.sock.sendall(b"x")
client.close()
"#,
        ])
        .arg(&peer.origin)
        .arg(peer.root.path().join("cert.pem"))
        .kill_on_drop(true)
        .status()
        .await
        .unwrap();
    assert!(client.success());
    assert_eq!(peer.lines.next_line().await.unwrap().as_deref(), Some("pending"));
    let report = peer.finish().await;
    assert_eq!(report["closed"], false);
    assert_eq!(report["requests"], 1);
    assert_eq!(report["discovery_credentials"], false);
    assert_eq!(report.get("write_attempted"), None);
    assert_eq!(report.get("peer_error"), None);
    println!("nonempty input: {report}");
}

#[test]
fn mcp_device_replay_capture_keeps_exact_initial_grant_without_a_writer() {
    use crate::lab_run::device::onboard::tests::{remote, world};
    let world = world();
    world.borrow_mut().grant = bytes(&record()).expose().clone();
    let initial = world.borrow().grant.clone();
    let mut runtime = Runtime::default();
    runtime.remote = Some(remote(&world));
    runtime.capture_mcp_replay(CONFIG).unwrap();
    world.borrow_mut().grant = b"changed".to_vec();
    runtime.capture_mcp_replay(CONFIG).unwrap();
    assert_eq!(runtime.replay.initial.as_ref().unwrap().expose(), &initial);
    assert!(!world.borrow().ran("push"));
    assert!(!runtime.replay.attempted);
}

#[tokio::test]
async fn t3_on_device_mcp_replay_refuses_missing_or_repeated_evidence_and_invalidates_counts() {
    let mut runtime = Runtime::default();
    assert_eq!(runtime.mcp_replay(CONFIG, "python3").await, Err(ERROR.into()));
    assert!(runtime.invalid);
    runtime.replay.attempted = true;
    assert_eq!(runtime.mcp_replay(CONFIG, "python3").await, Err(ERROR.into()));
    assert!(runtime.replay.initial.is_none());
}

fn ready_observer() -> echo_lab::lab_mcp::PortosObservations {
    let mut observer = echo_lab::lab_mcp::PortosObservations::new("portos", &["endpoint_query".into()]).unwrap();
    for (event, id) in [
        ("mcp.link.started", 1),
        ("mcp.link.approved", 1),
        ("mcp.refresh.committed", 2),
    ] {
        observer
            .trace(0, &json!({"event":event,"operation_id":id,"elapsed_ms":1}))
            .unwrap();
    }
    observer.restarted(1).unwrap();
    for row in [
        json!({"event":"mcp.grant.reloaded","elapsed_ms":1}),
        json!({"event":"mcp.refresh.committed","operation_id":1,"elapsed_ms":1}),
        json!({"event":"tools/call","elapsed_ms":1,"run_id":"run","thread_id":"thread",
            "turn_id":"turn","call_id":"call","name":"endpoint_query"}),
        json!({"event":"mcp.tool.completed","elapsed_ms":1,"provider_auth_mode":"chatgpt",
            "server":"portos","name":"endpoint_query","attempt_count":1,"outcome":"success"}),
    ] {
        observer.trace(1, &row).unwrap();
    }
    observer
}

#[test]
fn mcp_device_replay_observer_requires_completed_journey_and_rejects_duplicates() {
    let mut observer = echo_lab::lab_mcp::PortosObservations::new("portos", &["endpoint_query".into()]).unwrap();
    assert!(observer.replay_rejected().is_err());
    assert!(observer.measure().is_err());
    let mut observer = ready_observer();
    assert_eq!(observer.measure().unwrap().replay_rejected, 0);
    observer.replay_rejected().unwrap();
    assert_eq!(observer.measure().unwrap().replay_rejected, 1);
    assert!(observer.replay_rejected().is_err());
    assert!(observer.measure().is_err());
}

#[tokio::test]
async fn t3_on_device_mcp_replay_runtime_counts_only_tls_rejection_and_leaves_store_unchanged() {
    use crate::lab_run::device::onboard::tests::{remote, world};
    let peer = Peer::start("reject").await;
    let world = world();
    let mut old = record();
    old["issuer"] = peer.origin.clone().into();
    world.borrow_mut().grant = bytes(&old).expose().clone();
    world.borrow_mut().pem = peer.pem();
    let mut runtime = Runtime::default();
    runtime.remote = Some(remote(&world));
    runtime.generation = 2;
    runtime.portos = Some(ready_observer());
    runtime.capture_mcp_replay(CONFIG).unwrap();
    old["grant"]["refresh_token"] = "synthetic-current".into();
    let current = bytes(&old).expose().clone();
    world.borrow_mut().grant = current.clone();
    runtime.mcp_replay(CONFIG, "python3").await.unwrap();
    assert_eq!(runtime.portos.as_ref().unwrap().measure().unwrap().replay_rejected, 1);
    assert_eq!(world.borrow().grant, current);
    assert!(!world.borrow().ran("push"));
    assert_eq!(peer.finish().await["requests"], 2);
    assert_eq!(runtime.mcp_replay(CONFIG, "python3").await, Err(ERROR.into()));
    assert!(runtime.invalid);
}
