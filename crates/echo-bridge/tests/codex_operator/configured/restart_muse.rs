//! T3: real Settings dispatch and Codex wire across failed replacement recovery.
use super::super::tools::Media;
use super::*;
use echo_bridge::{bridge_settings::SettingsHost, bridge_voice::RuntimeCompletion};
use echo_config::{EffectiveConfig, MuseConfig, SettingsOverlay};
use echo_media::PeerState;
use std::sync::{Arc, OnceLock};

#[tokio::test]
#[ignore = "T3 requires Linux openssl and the local Codex operator fixture"]
async fn t3_settings_muse_clear_withdraws_next_catalog_after_failed_replacement() {
    let fixture = Fixture::new("chatgpt");
    let mut startup = settings_startup(&fixture);
    let muse = configure(&fixture, &mut startup);
    let base = startup.base.clone();
    let home = base.home.as_ref().unwrap().dir.clone();
    let trace = fixture.directory.path().join("runtime-muse");
    let mut runtime = Configured::from_effective(startup, trace.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let first = catalog_turn(&fixture, &mut runtime, None, true).await;
    assert!(first.restart_requested);
    let mut retained = first.settings.unwrap();
    let overlay = std::fs::read_to_string(home.join("settings.toml")).unwrap();
    assert_eq!(overlay, "[agent]\ndaily_request_cap = 8\n\n[muse]\nenabled = false\n");
    assert_eq!(base.muse, Some(muse.clone()));
    assert!(
        SettingsOverlay::from_toml_str(&overlay)
            .unwrap()
            .apply_to(&base)
            .unwrap()
            .muse
            .is_none()
    );
    assert!(muse.ca_file.is_file());
    assert_eq!(std::fs::read(&muse.credential_file).unwrap(), [b'x'; 32]);
    runtime.replacement_failed(&mut retained).unwrap();
    let recovered = catalog_turn(&fixture, &mut runtime, Some(retained), false).await;
    assert!(!recovered.restart_requested);
    let events = super::trace(&trace);
    for event in ["settings.reexec_failed", "settings.restart_recovered"] {
        assert_eq!(events.iter().filter(|row| row["event"] == event).count(), 1);
    }
    writeln!(
        std::io::stdout().lock(),
        "Muse catalog: ask_muse before=1 after_failed_exec=0; inherited config masked and base files retained"
    )
    .unwrap();
}

fn configure(fixture: &Fixture, startup: &mut EffectiveConfig) -> MuseConfig {
    let root = fixture.directory.path();
    let certificate = std::process::Command::new("openssl")
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
            "-keyout",
            "probe-key.pem",
            "-outform",
            "DER",
        ])
        .output()
        .unwrap();
    assert!(certificate.status.success());
    let muse = MuseConfig {
        url: "https://127.0.0.1:1".into(),
        ca_file: root.join("owner-ca.der"),
        credential_file: root.join("owner-muse"),
    };
    std::fs::write(&muse.ca_file, certificate.stdout).unwrap();
    std::fs::write(&muse.credential_file, [b'x'; 32]).unwrap();
    #[cfg(unix)]
    for file in [&muse.ca_file, &muse.credential_file] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    startup.base.home.as_mut().unwrap().agent_tools = echo_config::AgentTools {
        notes: false,
        prompts: false,
    };
    startup.base.mcp.server.clear();
    startup.base.muse = Some(muse.clone());
    startup.config = startup.overlay.apply_to(&startup.base).unwrap();
    muse
}

async fn catalog_turn(
    fixture: &Fixture,
    runtime: &mut Configured,
    retained: Option<Box<SettingsHost>>,
    before_clear: bool,
) -> RuntimeCompletion {
    let mode = if before_clear {
        "tool-muse-before"
    } else {
        "tool-muse-after"
    };
    std::fs::write(fixture.home.join("mode"), mode).unwrap();
    let observer = Observer::default();
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = runtime.execute_generation(
        FakeConnector::new(observer.clone()),
        async || {
            Ok(Media {
                state: tokio::sync::watch::channel(PeerState::New).0,
                closed: Arc::default(),
                setup: OnceLock::new(),
                lost: Arc::default(),
            })
        },
        vec![],
        async {
            let _ = stopping.await;
        },
        retained,
    );
    let journey = async {
        let (_control, _screen) = settings_ready(fixture, before_clear).await;
        observer.wake("catalog fixture", vec![]);
        while !observer.commands().contains(&Command::Stop) {
            tokio::task::yield_now().await;
        }
        let thread: Value =
            serde_json::from_slice(&std::fs::read(fixture.home.join("thread-config")).unwrap()).unwrap();
        let tools = thread["dynamicTools"].as_array().unwrap();
        assert_eq!(
            tools.iter().filter(|tool| tool["name"] == "ask_muse").count(),
            usize::from(before_clear)
        );
        if before_clear {
            clear(fixture).await;
            observer.finish(Ok(()));
        } else {
            observer.finish(Ok(()));
            stop.send(()).unwrap();
        }
    };
    let (completion, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, journey)
    }))
    .await
    .unwrap();
    super::super::host::released(fixture).await;
    std::fs::remove_file(fixture.home.join("peer-port")).unwrap();
    completion.unwrap()
}

async fn settings_ready(
    fixture: &Fixture,
    before_clear: bool,
) -> (tokio::net::TcpStream, Lines<BufReader<tokio::net::TcpStream>>) {
    let control = fixture.control().await;
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    let message = if before_clear {
        "Echo Live ready"
    } else {
        "Could not restart bridge; saved settings remain pending"
    };
    while bridge_wire_screen::state(&mut screen).await["message"] != message {}
    (control, screen)
}

async fn clear(fixture: &Fixture) {
    let url = settings_runtime_url(fixture, "/settings/v2");
    let client = reqwest::Client::new();
    let receipt = client
        .patch(url.clone())
        .json(&json!({"version":2,"settings":{"muse":{"clear":true}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(receipt.status(), reqwest::StatusCode::OK);
    assert_eq!(
        receipt.json::<Value>().await.unwrap(),
        json!({"version":2,"restart_pending":true})
    );
    let response = client.get(url).query(&[("include_muse", true)]).send().await.unwrap();
    let snapshot: Value = response.json().await.unwrap();
    assert_eq!(snapshot["muse"], json!({"configured":false,"reachability":"unchecked"}));
    assert_eq!(snapshot["restart_pending"], true);
}
