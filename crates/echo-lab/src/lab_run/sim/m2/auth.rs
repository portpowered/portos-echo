//! Synthetic private storage for the retained legacy voice-runtime fixture.
//! Canonical sign-in is app-server-owned; this fixture never imports credentials.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{process::Command, task::JoinHandle};

pub struct Auth {
    pub origin: String,
    pub refreshes: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Auth {
    pub async fn start() -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "m2: auth bind")?;
        let origin = format!("http://{}", listener.local_addr().map_err(|_| "m2: auth address")?);
        let refreshes = Arc::new(AtomicUsize::new(0));
        let count = refreshes.clone();
        let app = axum::Router::new().route(
            "/token",
            axum::routing::post(move || {
                count.fetch_add(1, Ordering::SeqCst);
                async {
                    (
                        axum::http::StatusCode::BAD_REQUEST,
                        axum::Json(json!({"error":"invalid_grant"})),
                    )
                }
            }),
        );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            origin,
            refreshes,
            task,
        })
    }

    pub async fn close(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for Auth {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn configure(command: &mut Command, root: &Path, case: &str, auth: &str) -> Result<(), String> {
    let directory = root.join("configured.private");
    let path = directory.join("bridge.toml");
    let config = std::fs::read_to_string(&path).map_err(|_| "m2: config missing")?;
    let config = config.replace("mode = \"api_key\"", "mode = \"chatgpt\"");
    std::fs::write(
        &path,
        format!("{config}\n[chatgpt_auth]\ntoken_endpoint = '{auth}/token'\n"),
    )
    .map_err(|_| "m2: cannot write auth config")?;
    std::fs::remove_file(directory.join("credential")).map_err(|_| "m2: cannot replace credential")?;
    let claims = json!({"exp":4_000_000_000_u64,
        "https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-local"}}); // lint-rules: allow-literal inert JWT claim key
    let token = format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(claims.to_string()));
    seed_legacy_store(&directory.join("credential"), &token)?;
    let credential = directory.join("credential");
    match case {
        "invalid" => std::fs::write(credential, "malformed synthetic store").map_err(|_| "m2: invalid store")?,
        "expired" => {
            let bytes = std::fs::read(&credential).map_err(|_| "m2: missing imported store")?;
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "m2: store format")?;
            value["expires"] = 1.into();
            value["last_refresh"] = 0.into();
            std::fs::write(credential, value.to_string()).map_err(|_| "m2: expired store")?;
        }
        "success" => {}
        _ => return Err("m2: unknown credential injection".into()),
    }
    command.arg("--sim-clock");
    Ok(())
}

// Temporary SUT setup, removed when the retained M2 voice path switches to Codex.
// Writes synthetic data only to the old fixture store, never CODEX_HOME/auth.json.
fn seed_legacy_store(path: &Path, token: &str) -> Result<(), String> {
    let record = json!({"access":token,"refresh":"synthetic-refresh",
        "account_id":"synthetic-local","expires":4_000_000_000_u64,
        "last_refresh":4_000_000_000_u64});
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| "m2: cannot create synthetic store")?;
    serde_json::to_writer(file, &record).map_err(|_| "m2: cannot write synthetic store".into())
}
