//! Actual pinned account discovery with a local-only backend and inert store.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use echo_bridge::{Error, bridge_voice::Configured};
use echo_codex::codex_supervisor::{Supervisor, SupervisorConfig};
use echo_link::fake::{Command as LinkCommand, FakeConnector, Observer};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

fn auth() -> Vec<u8> {
    let claims = json!({"exp":4_000_000_000_u64,"email":"local@example.invalid",
        "https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-local","chatgpt_plan_type":"plus"}}); // lint-rules: allow-literal inert synthetic JWT claim key
    let token = format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(claims.to_string()));
    serde_json::to_vec(&json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
        "tokens":{"id_token":token,"access_token":token,"refresh_token":"synthetic-refresh",
            "account_id":"synthetic-local"},"last_refresh":"2096-01-01T00:00:00Z"}))
    .unwrap()
}

fn rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
async fn event(path: &Path, name: &str) {
    let ceiling = std::time::Instant::now();
    while !rows(path).iter().any(|row| row["event"] == name) {
        assert!(
            ceiling.elapsed() < Duration::from_secs(8),
            "waiting for {name}: {:?}",
            rows(path)
        );
        tokio::task::yield_now().await;
    }
}
async fn health(path: &Path) -> Value {
    let rows = rows(path);
    let addr = rows.iter().find(|row| row["event"] == "bridge.started").unwrap()["health_addr"]
        .as_str()
        .unwrap();
    reqwest::get(format!("http://{addr}/healthz")) // lint-rules: allow-literal address obtained from an owned IPv4 loopback listener
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn configure(root: &Path, origin: &str) -> (echo_config::BridgeConfig, Vec<u8>) {
    let codex = super::voice::configure(root, 1);
    let home = codex.home.unwrap();
    drop(Supervisor::new(SupervisorConfig::new(binary(), home.clone())).unwrap());
    let auth = auth();
    std::fs::write(home.join("auth.json"), &auth).unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!("chatgpt_base_url = '{origin}/backend-api/'\n"),
    )
    .unwrap();
    let mut config = echo_config::BridgeConfig::load(root.join("bridge.toml")).unwrap();
    config.live.mode = LiveMode::Chatgpt;
    config.screen.pairing_token_file = root.join("screen-token");
    config.audit.log_dir = root.join("audit");
    (config, auth)
}

async fn serve(mut stream: TcpStream, unauthorized: bool) -> bool {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        request.push(stream.read_u8().await.unwrap());
        assert!(request.len() < 16384);
    }
    let request = String::from_utf8(request).unwrap();
    let first = request.lines().next().unwrap();
    let accounts = first.starts_with("GET /backend-api/wham/accounts/check ");
    assert!(
        accounts || first.starts_with("GET /backend-api/wham/config/bundle "),
        "unexpected provider path"
    );
    let (status, body) = if accounts && unauthorized {
        ("401 Unauthorized", json!({}))
    } else if accounts {
        (
            "200 OK",
            json!({"accounts":[{"id":"synthetic-local",
        "workspace_backend_origin":"https://chatgpt.com","account_routing_override":"NO_CONSTRAINT"}]}), // lint-rules: allow-literal inert pin routing metadata; all HTTP uses LocalRoutes proxy
        )
    } else {
        ("200 OK", json!({"requirements_toml":{}}))
    };
    let body = body.to_string();
    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    accounts
}

#[tokio::test]
async fn startup_network_actual_pin_refusal_recovers_and_unauthorized_still_relinks() {
    for unauthorized in [false, true] {
        exercise(unauthorized).await;
    }
}

async fn exercise(unauthorized: bool) {
    let _clock = MediaClock::start();
    let directory = tempfile::tempdir().unwrap();
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserved.local_addr().unwrap();
    let origin = format!("http://{address}"); // lint-rules: allow-literal address obtained from an owned IPv4 loopback listener
    let (config, auth) = configure(directory.path(), &origin);
    let home = config.codex.home.clone().unwrap();
    let path = directory.path().join("trace");
    let configured = Configured::new(config, path.clone(), ([127, 0, 0, 1], 0).into())
        .unwrap()
        .with_routes(LocalRoutes::new(&origin, &origin).unwrap());
    drop(reserved);
    let observer = Observer::default();
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = configured.execute(
        FakeConnector::new(observer.clone()),
        async || -> Result<Peer, Error> { unreachable!("no wake, no media") },
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = inspect(&path, address, &observer, unauthorized, stop);
    let (result, ()) = tokio::join!(work, inspect);
    assert_eq!(
        result,
        if unauthorized {
            Err(Error::Authorization)
        } else {
            Ok(())
        }
    );
    assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth);
    drop(Supervisor::new(SupervisorConfig::new(binary(), home)).unwrap());
    let rows = rows(&path);
    let count = |name: &str| rows.iter().filter(|row| row["event"] == name).count();
    assert_eq!(count("bridge.recovering"), 1);
    assert_eq!(count("bridge.ready"), usize::from(!unauthorized));
    assert_eq!(count("auth.relink_required"), usize::from(unauthorized));
    assert_eq!(count("health.released"), 1);
    assert_eq!(count("screen.released"), 1);
}

async fn inspect(
    path: &Path,
    address: std::net::SocketAddr,
    observer: &Observer,
    unauthorized: bool,
    stop: tokio::sync::oneshot::Sender<()>,
) {
    event(path, "bridge.recovering").await;
    assert!(observer.commands().is_empty());
    let state = health(path).await;
    assert_eq!(state["relink_required"], false);
    assert_eq!(state["startup_waiting_reason"], "dependency_unavailable");
    let listener = TcpListener::bind(address).await.unwrap();
    let accounts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ledger = accounts.clone();
    let serving = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            ledger.fetch_add(
                usize::from(serve(stream, unauthorized).await),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
    });
    // Cross the timer wheel's millisecond rounding at the 1 s retry boundary.
    tokio::time::advance(Duration::from_millis(1001)).await;
    event(
        path,
        if unauthorized {
            "auth.relink_required"
        } else {
            "bridge.ready"
        },
    )
    .await;
    let state = health(path).await;
    assert_eq!(state["relink_required"], unauthorized);
    assert!(state.get("startup_waiting_reason").is_none());
    assert_eq!(observer.commands().contains(&LinkCommand::Probe), !unauthorized);
    assert!(accounts.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    stop.send(()).unwrap();
    serving.abort();
    let _ = serving.await;
}
