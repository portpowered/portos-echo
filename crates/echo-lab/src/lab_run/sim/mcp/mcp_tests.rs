//! T3 component with delivered bridge/pin/media/OAuth/MCP and controlled Wyoming.
use super::*;

#[tokio::test]
async fn t3_m5_codex_provider_quiesces_and_closes_without_admitting_a_call() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let provider = super::super::mcp_peers::Peers::bind(python).await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        provider.quiesce().await.unwrap();
        provider.shutdown().await.unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        result,
        json!({"event":"provider.snapshot","calls":[],"sidebands":0,
        "external":0,"delegations":[],"models":[],"appends":[],"transcripts":[]})
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and offline Codex pin"]
async fn m5_codex_link_refresh_restart_call_replay_and_unlink_keep_exact_counts() {
    let phase = std::cell::Cell::new("fixture startup");
    let result = tokio::time::timeout(std::time::Duration::from_secs(25), Box::pin(journey(&phase))).await;
    assert!(result.is_ok(), "M5: component deadline during {}", phase.get());
}

async fn journey(phase: &std::cell::Cell<&str>) {
    let binary = std::env::current_exe()
        .unwrap()
        .ancestors()
        .find(|path| matches!(path.file_name().and_then(|name| name.to_str()), Some("deps" | "build")))
        .and_then(Path::parent)
        .unwrap()
        .join(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
    let options = crate::lab_run::run_options::parse(&[
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/scenarios/m5-port-os-link.yaml")
            .to_string_lossy()
            .into_owned(),
        "--driver".into(),
        "sim".into(),
        "--bridge".into(),
        binary.to_string_lossy().into_owned(),
    ])
    .unwrap();
    let mut session = Session::default();
    session.prepare(&options).unwrap();
    let path = session.directory.clone();
    let mut command = tokio::process::Command::new(&options.python);
    command
        .arg("-u")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/mcp/mcp_host.py"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/wyoming/wake-word.jsonl"));
    session.host = Some(Process::start(&mut command).unwrap());
    let ready = session.host.as_mut().unwrap().expect("host.ready").await.unwrap();
    let address = ready["address"].as_str().unwrap().parse().unwrap();
    let origin = session.start_peers(address, &options.python).await.unwrap();
    phase.set("bridge startup");
    session.launch(&origin).await.unwrap();
    let result = linked(&mut session, phase).await;
    phase.set("cleanup");
    let cleanup = session.close().await;
    assert_eq!(result, Ok(()));
    assert_eq!(cleanup, Ok(()));
    assert_eq!(
        session.final_measurement(),
        Some(LinkCounts {
            link: 1,
            tools_list: 1,
            tool_call: 1,
            forced_refresh: 2,
            replay_rejected: 1,
            relink_error: 1,
            restart_grant_kept: 1,
            unlink: 1
        })
    );
    phase.set("endpoint release");
    released(&session.endpoints).await;
    assert!(!path.exists());
}

async fn released(endpoints: &[SocketAddr]) {
    // Refused connects can complete slowly on Windows. Observe each independent
    // listener concurrently, preserving every release assertion and the ceiling.
    let mut checks = tokio::task::JoinSet::new();
    for &address in endpoints {
        checks.spawn(tokio::net::TcpStream::connect(address));
    }
    while let Some(result) = checks.join_next().await {
        assert!(result.unwrap().is_err());
    }
}

async fn linked(session: &mut Session, phase: &std::cell::Cell<&str>) -> Result<(), String> {
    phase.set("link");
    session.link().await?;
    phase.set("approve");
    session.approve().await?;
    session.operator("--refresh-now", "MCP refreshed\n").await?;
    phase.set("restart");
    session.restart().await?;
    session.operator("--refresh-now", "MCP refreshed\n").await?;
    phase.set("call");
    session.call().await?;
    phase.set("replay");
    session.replay().await?;
    phase.set("probe");
    session.probe().await?;
    session.operator("unlink", "MCP unlinked\n").await?;
    session.observe()
}
