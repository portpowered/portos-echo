use crate::service_support::{Fixture, empty, handler, progress, reject};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

#[tokio::test]
async fn f08_gadget_exit_values_never_infer_completion_or_leak_diagnostics() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    for code in [3, 78, 9] {
        fixture.script("Still working\n", "private gadget diagnostic", code, None);
        let id = fixture.submit(&daemon).await;
        let event = if code == 3 {
            json!({"type":"progress","task_id":id,"message":"Still working\n"})
        } else {
            json!({"type":"failed","task_id":id,"retryable":false,"message":"Muse gadget request failed"})
        };
        assert_eq!(fixture.event(&daemon, &id).await, (200, event));
        assert_eq!(fixture.event(&daemon, &id).await, empty());
        if code != 3 {
            fixture.rejected(&id, "task_id already reported").await;
        }
    }
    assert_eq!(fixture.calls().len(), 3);
    daemon.stop(true).await;
}

#[tokio::test]
async fn f12_failed_dequeue_and_submit_are_storage_errors_never_empty_success() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let id = fixture.submit(&daemon).await;
    // Replacing the target with a directory deterministically breaks atomic
    // replacement even when tests run as root; chmod-based failures do not.
    std::fs::remove_file(fixture.path("tasks.json")).unwrap();
    std::fs::create_dir(fixture.path("tasks.json")).unwrap();
    assert_eq!(
        fixture.event(&daemon, &id).await,
        (503, json!({"error":"storage_unavailable"}))
    );
    assert_eq!(
        fixture
            .request(&daemon, "POST", "/v1/tasks", Some(crate::service_support::task()))
            .await,
        (503, json!({"error":"storage_unavailable"}))
    );
    assert_eq!(fixture.calls().len(), 1);
    daemon.stop(false).await;
}

#[tokio::test]
async fn f12_report_replacement_fault_rejects_before_acknowledgement() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let id = fixture.submit(&daemon).await;
    std::fs::remove_file(fixture.path("tasks.json")).unwrap();
    std::fs::create_dir(fixture.path("tasks.json")).unwrap();
    fixture
        .rejected(&id, "report persistence failed: store I/O failed")
        .await;
    assert_eq!(
        fixture.event(&daemon, &id).await,
        (503, json!({"error":"storage_unavailable"}))
    );
    daemon.stop(false).await;
}

#[tokio::test]
async fn f13_f15_handler_checks_disconnect_envelope_identity_and_bounds() {
    let fixture = Fixture::new();
    reject(&fixture.report(b"{}").await, "report transport unavailable");
    let input = br#"{"task_id":"task-x","status":"completed","summary":"x"}"#;
    for reply in [
        None,
        Some(br#"{"version":1,"output":{"accepted":true,"task_id":"wrong"}}"#.as_slice()),
        Some(b"{".as_slice()),
    ] {
        let socket = fixture.path("peer.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let client = handler(&socket, input);
        let peer = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).await.unwrap();
            if let Some(reply) = reply {
                stream.write_u32(u32::try_from(reply.len()).unwrap()).await.unwrap();
                stream.write_all(reply).await.unwrap();
            }
        };
        let (output, ()) = tokio::join!(client, peer);
        reject(
            &output,
            if reply.is_none() {
                "report transport unavailable"
            } else {
                "invalid report response"
            },
        );
        std::fs::remove_file(socket).unwrap();
    }
    let socket = fixture.path("oversize.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        stream.write_u32(1_048_577).await.unwrap();
    };
    let (output, ()) = tokio::join!(handler(&socket, input), peer);
    reject(&output, "report response too large");
}

#[tokio::test]
async fn f14_private_frames_and_exact_input_limit() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let id = fixture.submit(&daemon).await;
    for request in [
        vec![0, 0, 0, 0],
        vec![0, 1, 0, 1],
        vec![0, 0, 0, 2, b'{'],
        vec![0, 0, 0, 2, b'{', b'}', b'x'],
    ] {
        let mut stream = UnixStream::connect(fixture.path("report.sock")).await.unwrap();
        stream.write_all(&request).await.unwrap();
        stream.shutdown().await.unwrap();
        let length = stream.read_u32().await.unwrap();
        let mut bytes = vec![0; length as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"version":1,"error":"invalid report frame"})
        );
    }
    reject(&fixture.report(&vec![b'x'; 65537]).await, "report input too large");
    let base = json!({"task_id":id,"status":"completed","summary":""}).to_string();
    let summary = "s".repeat(65536 - base.len());
    let input = json!({"task_id":id,"status":"completed","summary":summary}).to_string();
    assert_eq!(input.len(), 65536);
    let output = fixture.report(input.as_bytes()).await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        json!({"accepted":true,"task_id":id}).to_string().as_bytes()
    );
    assert_eq!(output.stderr, b"");
    assert_eq!(fixture.event(&daemon, &id).await, progress(&id));
    assert_eq!(
        fixture.event(&daemon, &id).await,
        (200, json!({"type":"completed","task_id":id,"summary":summary}))
    );
    daemon.stop(true).await;
}

#[tokio::test]
async fn f15_handler_stalled_peer_and_stdin_have_total_lifetime_ceiling() {
    use std::process::Stdio;
    let fixture = Fixture::new();
    let socket = fixture.path("stalled.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        let mut probe = [0];
        // Read readiness from the actual handler disconnect, not a fixed sleep.
        assert_eq!(stream.read(&mut probe).await.unwrap(), 0);
        stream
    };
    let (output, stream) = tokio::join!(handler(&socket, b"{}"), peer);
    reject(&output, "report transport timed out");
    drop(stream);
    let mut child = tokio::process::Command::new(crate::service_support::HANDLER)
        .env("ECHO_MUSE_REPORT_SOCKET", &socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let output = tokio::time::timeout(Duration::from_secs(9), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    drop(stdin);
    reject(&output, "report input timed out");
}

#[tokio::test]
async fn f16_tls_verifies_trust_hostname_and_rejects_plaintext() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let untrusted = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    assert!(untrusted.get(format!("{}/v1/tasks", daemon.url)).send().await.is_err());
    let address = daemon
        .url
        .trim_start_matches("https://")
        .parse::<std::net::SocketAddr>()
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            std::fs::read(fixture.path("cert.der")).unwrap(),
        ))
        .unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let peer = tokio::net::TcpStream::connect(address).await.unwrap();
    let wrong_name = rustls::pki_types::ServerName::try_from("wrong.test").unwrap();
    assert!(
        tokio_rustls::TlsConnector::from(std::sync::Arc::new(tls))
            .connect(wrong_name, peer)
            .await
            .is_err()
    );
    let address = daemon.url.trim_start_matches("https://");
    let mut raw = tokio::net::TcpStream::connect(address).await.unwrap();
    raw.write_all(b"GET /v1/tasks HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut bytes = Vec::new();
    let _closed = tokio::time::timeout(Duration::from_secs(3), raw.read_to_end(&mut bytes))
        .await
        .unwrap();
    assert!(!bytes.starts_with(b"HTTP/"));
    assert_eq!(fixture.calls(), Vec::<Value>::new());
    daemon.stop(true).await;
}

#[tokio::test]
async fn f12_submit_commit_failure_has_zero_gadget_admission() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    std::fs::remove_file(fixture.path("tasks.json")).unwrap();
    std::fs::create_dir(fixture.path("tasks.json")).unwrap();
    assert_eq!(
        fixture
            .request(&daemon, "POST", "/v1/tasks", Some(crate::service_support::task()))
            .await,
        (503, json!({"error":"storage_unavailable"}))
    );
    assert_eq!(fixture.calls(), Vec::<Value>::new());
    daemon.stop(false).await;
}

#[tokio::test]
async fn f19_gadget_output_limit_fails_safely_for_each_stream() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    for stderr in [false, true] {
        let large = "x".repeat(98305);
        fixture.script(
            if stderr { "" } else { &large },
            if stderr { &large } else { "" },
            0,
            None,
        );
        let id = fixture.submit(&daemon).await;
        assert_eq!(
            fixture.event(&daemon, &id).await,
            (
                200,
                json!({"type":"failed","task_id":id,
            "retryable":false,"message":"Muse gadget request failed"})
            )
        );
        assert_eq!(fixture.event(&daemon, &id).await, empty());
        fixture.rejected(&id, "task_id already reported").await;
    }
    assert_eq!(fixture.calls().len(), 2);
    daemon.stop(true).await;
}
