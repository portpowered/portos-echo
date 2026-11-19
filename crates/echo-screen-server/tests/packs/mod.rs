//! T3: production background installer against a synthetic local HTTPS peer.
//! Python/OpenSSL are fixture owners, never a real account or internet endpoint.
use crate::support::{Result, request, server};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

struct Peer {
    process: Child,
    output: BufReader<tokio::process::ChildStdout>,
    root: tempfile::TempDir,
    base: String,
    archive: Vec<u8>,
}

const PEER: &str = r"
import http.server, pathlib, ssl, sys
root = pathlib.Path(sys.argv[1])
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        if self.path == '/stall':
            self.send_response(200)
            self.send_header('Content-Length', '100')
            self.end_headers()
            self.wfile.flush()
            print('stalled', flush=True)
            self.rfile.read(1)
            return
        if self.path == '/http':
            self.send_response(302)
            self.send_header('Location', 'http://127.0.0.1/forbidden')
            self.end_headers()
            return
        if self.path == '/loop':
            self.send_response(302)
            self.send_header('Location', '/loop')
            self.end_headers()
            return
        body = (root / 'pack.tar.gz').read_bytes()
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(root / 'ca.pem', root / 'key.pem')
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_port, flush=True)
server.serve_forever()
";

async fn certificate(root: &Path, name: &str) -> Result {
    let output = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048", // lint-rules: allow-literal OpenSSL algorithm and bit count, not a network destination
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=synthetic-pack-peer",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-keyout",
            "key.pem",
            "-out",
            name,
        ])
        .current_dir(root)
        .output()
        .await?;
    assert!(output.status.success(), "synthetic certificate generation failed");
    Ok(())
}

fn archive() -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let manifest = br#"{"format":1,"name":"template","version":"0.1.0","character":{"rest":"rest.png"}}"#;
    for (name, bytes) in [
        ("pack.json", manifest.as_slice()),
        ("rest.png", b"synthetic-rest".as_slice()),
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, name, bytes).unwrap();
    }
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&builder.into_inner().unwrap()).unwrap();
    gzip.finish().unwrap()
}

impl Peer {
    async fn start() -> Result<Self> {
        let root = tempfile::tempdir()?;
        certificate(root.path(), "ca.pem").await?;
        let archive = archive();
        std::fs::write(root.path().join("pack.tar.gz"), &archive)?;
        let mut process = Command::new("python3")
            .args(["-u", "-c", PEER])
            .arg(root.path())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut line = String::new();
        let mut output = BufReader::new(process.stdout.take().ok_or("peer stdout")?);
        output.read_line(&mut line).await?;
        let port: u16 = line.trim().parse()?;
        let base = "https://127.0.0.1";
        Ok(Self {
            process,
            output,
            root,
            base: format!("{base}:{port}"),
            archive,
        })
    }

    fn ca(&self) -> std::path::PathBuf {
        self.root.path().join("ca.pem")
    }
    fn digest(&self) -> String {
        Sha256::digest(&self.archive)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    async fn stop(mut self) -> Result {
        self.process.kill().await?;
        self.process.wait().await?;
        Ok(())
    }

    async fn stalled(&mut self) -> Result {
        let mut line = String::new();
        self.output.read_line(&mut line).await?;
        assert_eq!(line.trim(), "stalled");
        Ok(())
    }
}

#[tokio::test]
async fn t3_screen_pack_trusted_https_installs_bad_digest_retains_server_and_prior_data() -> Result {
    let peer = Peer::start().await?;
    let home = tempfile::tempdir()?;
    let (mut server, states) = server().await?;
    let mut jobs = server.install_configured_packs(
        home.path().into(),
        Some(peer.ca()),
        vec![
            (format!("{}/good", peer.base), peer.digest()),
            (format!("{}/good", peer.base), "0".repeat(64)),
            (format!("{}/http", peer.base), peer.digest()),
            (format!("{}/loop", peer.base), peer.digest()),
        ],
    );
    let good = jobs.recv().await.ok_or("missing install result")?;
    assert_eq!(good.installed.as_deref(), Some("template@0.1.0"));
    assert_eq!(good.reason, None);
    let bad = jobs.recv().await.ok_or("missing digest result")?;
    assert_eq!(bad.installed, None);
    assert_eq!(bad.reason.as_deref(), Some("sha256 mismatch or invalid digest"));
    assert_eq!(
        jobs.recv().await.ok_or("missing redirect result")?.reason.as_deref(),
        Some("expected HTTPS URL without credentials or fragment")
    );
    assert_eq!(
        jobs.recv()
            .await
            .ok_or("missing redirect cap result")?
            .reason
            .as_deref(),
        Some("download failed")
    );
    assert!(jobs.recv().await.is_none());
    assert_eq!(
        std::fs::read(home.path().join("screens/template@0.1.0/rest.png"))?,
        b"synthetic-rest"
    );
    assert_eq!(std::fs::read_dir(home.path().join("screens"))?.count(), 2);
    assert_eq!(request(server.bound_addr, "GET", "/screen.css", "", "").await?.0, 200);
    drop(states);
    server.shutdown().await?;
    peer.stop().await
}

#[tokio::test]
async fn t3_screen_pack_exclusive_ca_and_missing_ca_fail_without_extracting_or_stopping() -> Result {
    let peer = Peer::start().await?;
    let unrelated = tempfile::tempdir()?;
    certificate(unrelated.path(), "unrelated.pem").await?;
    for (ca, expected) in [
        (None, "configured CA bundle unavailable"),
        (Some(unrelated.path().join("unrelated.pem")), "download failed"),
    ] {
        let home = tempfile::tempdir()?;
        let (mut server, states) = server().await?;
        let mut jobs = server.install_configured_packs(
            home.path().into(),
            ca,
            vec![(format!("{}/good", peer.base), peer.digest())],
        );
        let result = jobs.recv().await.ok_or("missing safe failure")?;
        assert_eq!(result.installed, None);
        assert_eq!(result.reason.as_deref(), Some(expected));
        assert!(!home.path().join("screens/template@0.1.0").exists());
        assert_eq!(request(server.bound_addr, "GET", "/screen.css", "", "").await?.0, 200);
        drop(states);
        server.shutdown().await?;
    }
    peer.stop().await
}

#[tokio::test]
async fn t3_screen_pack_shutdown_cancels_stalled_transfer_and_removes_temporary_bytes() -> Result {
    let mut peer = Peer::start().await?;
    let home = tempfile::tempdir()?;
    let (mut server, states) = server().await?;
    let mut jobs = server.install_configured_packs(
        home.path().into(),
        Some(peer.ca()),
        vec![(format!("{}/stall", peer.base), peer.digest())],
    );
    peer.stalled().await?;
    server.shutdown().await?;
    assert!(jobs.recv().await.is_none());
    assert_eq!(std::fs::read_dir(home.path().join("screens"))?.count(), 0);
    drop(states);
    peer.stop().await
}

#[tokio::test]
async fn t3_screen_pack_fake_clock_deadline_retains_serving_process() -> Result {
    let mut peer = Peer::start().await?;
    let home = tempfile::tempdir()?;
    let (mut server, states) = server().await?;
    let mut jobs = server.install_configured_packs(
        home.path().into(),
        Some(peer.ca()),
        vec![(format!("{}/stall", peer.base), peer.digest())],
    );
    peer.stalled().await?;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert_eq!(
        jobs.recv().await.ok_or("missing deadline result")?.reason.as_deref(),
        Some("download deadline exceeded")
    );
    tokio::time::resume();
    assert_eq!(std::fs::read_dir(home.path().join("screens"))?.count(), 0);
    assert_eq!(request(server.bound_addr, "GET", "/screen.css", "", "").await?.0, 200);
    drop(states);
    server.shutdown().await?;
    peer.stop().await
}
