//! Component T3: real pinned host, configured prebuilt bridge, controlled Live.
use super::*;
use std::io::Write;
use tokio::net::TcpListener;
pub(crate) mod continuous;
mod journeys;

async fn refusal(cause: &str) {
    let directory = tempfile::tempdir().unwrap();
    let mut host = Process::spawn(&mut host_script("crates/echo-bridge/tests/bridge_refusal/host.sh"));
    let port = host.line().await.parse().unwrap();
    bridge_wire_screen::setup(directory.path(), port);
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Http {
            status: if cause == "relink-required" { 401 } else { 503 },
            body: "synthetic private body".into(),
            location: None,
        },
    })
    .await
    .unwrap();
    if cause == "kill-switch" {
        bridge_wire_auth::invoke(
            directory.path(),
            &["kill", "--config", "bridge.toml"],
            0,
            "kill-switch: on\n",
        )
        .await;
    }
    let origin = fake.base_url().trim_end_matches('/').to_owned();
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(directory.path(), &origin));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    for wake in 1..=2 {
        let result: Value = serde_json::from_str(&host.command("wake").await).unwrap();
        assert_refusal(&result, cause, wake);
        if cause == "relink-required" {
            std::fs::write(directory.path().join("credential"), "synthetic-repaired-key").unwrap();
        }
    }
    verify_surfaces(directory.path(), cause, fake.observer().calls().unwrap().len()).await;
    bridge.finish(0).await;
    host.finish(0).await;
    fake.shutdown().await;
}

async fn verify_surfaces(directory: &Path, cause: &str, calls: usize) {
    assert_eq!(
        calls,
        match cause {
            "kill-switch" => 0,
            "relink-required" => 1,
            _ => 2,
        }
    );
    let path = directory.join("trace");
    if cause != "kill-switch" {
        bridge_wire_auth_failure::assert_surfaces(&path, cause == "relink-required").await;
    }
    assert_eq!(
        trace(&path)
            .iter()
            .filter(|row| row["event"] == "reply.started")
            .count(),
        0
    );
}

fn assert_refusal(result: &Value, cause: &str, wake: usize) {
    let code = format!("echo-{cause}");
    assert_eq!(result["errors"].as_array().unwrap().len(), 1);
    assert_eq!(result["total_errors"], wake);
    assert_eq!(result["errors"][0]["data"]["code"], code);
    assert_eq!(result["connections"], 1);
    assert_eq!(result["disconnections"], 0);
    assert_eq!(result["pcm_bytes"], 0);
    let elapsed = result["refusal_to_idle_ms"].as_f64().unwrap();
    writeln!(std::io::stdout(),
        "cause={cause} wake={wake} code={code} refusal_to_idle_ms={elapsed:.3} server_error=true same_connection=true listening_ms={}",
        result["listening_ms"]
    ).unwrap();
    assert!(elapsed <= 1000.0, "refusal exceeded 1000 ms: {elapsed}");
}

pub(super) async fn error(satellite: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin), relink: bool) {
    let (header, payload) = bridge_short::read_reply(satellite).await;
    assert_eq!(header["type"], "error");
    let (code, text) = if relink {
        ("echo-relink-required", "Relink required.")
    } else {
        ("echo-unavailable", "Live unavailable.")
    };
    assert_eq!(header["data"], json!({"code":code,"text":text}));
    assert_eq!(payload, Vec::<u8>::new());
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_refusal_t3_relink() {
    refusal("relink-required").await;
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_refusal_t3_kill() {
    refusal("kill-switch").await;
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_refusal_t3_unavailable() {
    refusal("unavailable").await;
}

pub(super) fn host_script(script: &str) -> Command {
    let shell = if cfg!(windows) {
        let result = std::process::Command::new("python")
            .arg(root().join("scripts/make-posix.py"))
            .output()
            .unwrap();
        assert!(result.status.success());
        PathBuf::from(String::from_utf8(result.stdout).unwrap().trim()).join("sh.exe")
    } else {
        "sh".into()
    };
    let mut command = Command::new(&shell);
    if cfg!(windows) {
        let inherited = std::env::var_os("PATH").unwrap();
        let paths = std::iter::once(shell.parent().unwrap().to_path_buf()).chain(std::env::split_paths(&inherited));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }
    command.arg(script).current_dir(root());
    command
}

pub(super) async fn setup_clear_and_reply(
    directory: &Path,
    satellite: &mut bridge_short::Satellite,
    listener: TcpListener,
    bridge: &mut Process,
) {
    bridge_wire_auth::invoke(
        directory,
        &["kill", "--clear", "--config", "bridge.toml"],
        0,
        "kill-switch: off\n",
    )
    .await;
    let live = bridge_peer::Live::bind_listener(listener, 3, None);
    bridge_short::send(
        satellite,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
    )
    .await;
    assert_eq!(bridge_short::read_reply(satellite).await.0["type"], "audio-start");
    bridge_short::receive_short(satellite, &live, &directory.join("trace"), 3).await;
    bridge_short::send(satellite, "played", json!({})).await;
    bridge_short::send(satellite, "audio-stop", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-3"})
    );
    assert_eq!(live.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
