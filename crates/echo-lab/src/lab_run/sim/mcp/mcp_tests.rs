//! T3 component with delivered bridge/pin/media/OAuth/MCP and controlled Wyoming.
use super::*;
use echo_spoof_mcp::Options as SpoofOptions;
use std::io::Write as _;

#[tokio::test]
async fn t3_m5_codex_provider_quiesces_and_closes_without_admitting_a_call() {
    let python = "python3";
    let provider = super::super::mcp_peers::Peers::bind(python, Journey::Lifecycle, None)
        .await
        .unwrap();
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
    let options = options("m5-port-os-link");
    let mut session = Session::default();
    session.prepare(&options).unwrap();
    let path = session.directory.clone();
    let mut command = tokio::process::Command::new(&options.python);
    command
        .arg("-u")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/mcp/mcp_host.py"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/wyoming/wake-word.jsonl"));
    session.host = Some(Process::start_observed(&mut command).unwrap());
    let ready = session.host.as_mut().unwrap().expect("host.ready").await.unwrap();
    let address = ready["address"].as_str().unwrap().parse().unwrap();
    let origin = session.start_peers(address, &options.python).await.unwrap();
    assert_eq!(session.released, [] as [&str; 0]);
    phase.set("bridge startup");
    session.launch(&origin).await.unwrap();
    assert_eq!(session.released, [] as [&str; 0]);
    let result = linked(&mut session, phase).await;
    phase.set("cleanup");
    assert_eq!(session.released, ["bridge"]); // only the pre-restart generation collected
    let cleanup = session.close().await;
    assert_eq!(result, Ok(()));
    assert_eq!(cleanup, Ok(()), "M5: collected owners: {:?}", session.released);
    assert_closed(&session);
    assert!(!path.exists());
}

fn options(scenario: &str) -> Options {
    let binary = std::env::current_exe()
        .unwrap()
        .ancestors()
        .find(|path| matches!(path.file_name().and_then(|name| name.to_str()), Some("deps" | "build")))
        .and_then(Path::parent)
        .unwrap()
        .join("echo-bridge");
    crate::lab_run::run_options::parse(&[
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../tests/scenarios/{scenario}.yaml"))
            .to_string_lossy()
            .into_owned(),
        "--driver".into(),
        "sim".into(),
        "--bridge".into(),
        binary.to_string_lossy().into_owned(),
    ])
    .unwrap()
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge, offline Codex pin and hassmic-host"]
async fn t3_portos_read_only_delegation_replies_to_relink_without_retry() {
    let observed = portos_journey(Journey::ReadOnly, None).await;
    assert_eq!(observed["reply"], "Link Echo to Port OS again to control devices.");
    assert_eq!(observed["health"], "relink_for_scope");
    writeln!(std::io::stdout(), "{}", observed["diagnostics"][0]).unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge, offline Codex pin and hassmic-host"]
async fn t3_portos_missing_or_ambiguous_rooms_do_not_dispatch_control() {
    for (fault, reply) in [
        (HomeFault::NoRoom, "I could not find Twin room."),
        (HomeFault::AmbiguousRoom, "Which Twin room did you mean?"),
    ] {
        let observed = portos_journey(Journey::Twin, Some(fault)).await;
        assert_eq!(observed["reply"], reply);
        assert_eq!(observed["home"]["calls"].as_array().unwrap().len(), 1);
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge, offline Codex pin and hassmic-host"]
async fn t3_portos_control_service_error_never_retries_or_claims_success() {
    let observed = portos_journey(Journey::Twin, Some(HomeFault::ControlUnavailable)).await;
    assert_eq!(
        observed["reply"],
        "I could not confirm the Twin room lights were turned off."
    );
    assert_eq!(observed["home"]["lights_on"], json!([true, true]));
    assert_eq!(observed["home"]["calls"].as_array().unwrap().len(), 2);
}

async fn portos_journey(journey: Journey, fault: Option<HomeFault>) -> Value {
    let options = options("m5-twin-room-lights");
    let mut session = Session::default().for_journey(journey);
    session.home_fault = fault;
    let root = tempfile::tempdir().unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(24), async {
        session.start(&options, root.path()).await?;
        session.link().await?;
        session.approve().await?;
        session.lights("Computer, turn off twin room lights").await
    })
    .await;
    let cleanup = session.close().await;
    assert_eq!(cleanup, Ok(()), "Port OS journey {result:?}");
    assert_eq!(session.released, ["bridge", "provider", "authorization", "host"]);
    result.expect("Port OS control journey timed out").unwrap()
}

fn assert_closed(session: &Session) {
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
    assert_eq!(
        session.released,
        ["bridge", "bridge", "provider", "spoof", "authorization", "host"]
    );
    assert!(session.bridge.is_none() && session.host.is_none());
    assert!(session.live.is_none() && session.spoof.is_none() && session.fixture.is_none());
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

#[tokio::test]
async fn t3_m5_cleanup_observations_belong_to_each_joined_owner() {
    let mut first = Session::default();
    first.spoof = Some(Spoof::start(SpoofOptions::default()).await.unwrap());
    let mut retained = Session::default();
    retained.spoof = Some(Spoof::start(SpoofOptions::default()).await.unwrap());
    let address = endpoint(retained.spoof.as_ref().unwrap().url()).unwrap();
    assert!(first.released.is_empty() && retained.released.is_empty());
    assert_eq!(first.close().await, Ok(()));
    assert_eq!(first.released, ["spoof"]);
    assert_eq!(retained.released, [] as [&str; 0]);
    // This owner is still listening; the first session's completion proves
    // nothing about it. This live connection never probes a released address.
    let _connection = tokio::net::TcpStream::connect(address).await.unwrap();
    assert_eq!(retained.close().await, Ok(()));
    assert_eq!(retained.released, ["spoof"]);
    assert_eq!(first.close().await, Ok(()));
    assert_eq!(first.released, ["spoof"]);
}

#[tokio::test]
async fn t3_m5_failed_child_collection_does_not_report_successful_release() {
    let mut command = tokio::process::Command::new("python3");
    command.args([
        "-u",
        "-c",
        "import sys; print('{\"event\":\"host.ready\"}'); sys.stderr.write('private-fixture-data\\n' * 4096); sys.stderr.flush(); sys.stdin.read(); sys.exit(7)",
    ]);
    let mut host = Process::start_observed(&mut command).unwrap();
    host.expect("host.ready").await.unwrap();
    let collected = host.observe_collection();
    let mut session = Session::default();
    session.host = Some(host);
    session.spoof = Some(Spoof::start(SpoofOptions::default()).await.unwrap());
    let closed = session.close().await;
    let status = collected.try_recv().unwrap();
    assert_eq!(status.code(), Some(7));
    assert_eq!(
        closed,
        Err(format!(
            "M5 host: cleanup: owned child exited nonzero; {status}; forced=false; diagnostic=unrecognized stderr"
        ))
    );
    assert_eq!(session.released, ["spoof"]);
    assert!(session.host.is_none() && session.spoof.is_none());
}

impl Session {
    pub(super) fn record_release<T, E>(&mut self, owner: &'static str, result: &Result<T, E>) {
        // Successful shutdown APIs join the actual serving tasks; Process::close
        // awaits this instance's child. Never record intention or a failed join.
        if result.is_ok() {
            self.released.push(owner);
        }
    }
}
