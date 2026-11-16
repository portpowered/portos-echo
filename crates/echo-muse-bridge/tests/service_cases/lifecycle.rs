use crate::service_support::{CHAT, DAEMON, Fixture, empty, progress};
use serde_json::{Value, json};
use std::{os::unix::fs::MetadataExt, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    process::Command,
};

#[tokio::test]
async fn f09_f10_restarts_keep_live_terminal_and_tombstone_state_without_replay() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start().await;
    let live = fixture.submit(&daemon).await;
    let pending = fixture.submit(&daemon).await;
    let cancelled = fixture.submit(&daemon).await;
    let expired = fixture.submit(&daemon).await;
    for id in [&live, &pending, &expired] {
        assert_eq!(fixture.event(&daemon, id).await, progress(id));
    }
    fixture.accepted(&pending, "completed", "pending").await;
    assert_eq!(
        fixture
            .request(&daemon, "POST", &format!("/v1/tasks/{cancelled}/cancel"), None)
            .await,
        empty()
    );
    daemon.time(160).await;
    assert_eq!(fixture.event(&daemon, &expired).await.1["type"], "failed");
    daemon.stop(true).await;
    let daemon = fixture.start().await;
    // Restart restores the configured controlled clock (100), while snapshots
    // retain absolute deadlines and stored expiry/cancel tombstones.
    fixture.accepted(&live, "completed", "light is on").await;
    assert_eq!(
        fixture.event(&daemon, &live).await,
        (200, json!({"type":"completed","task_id":live,"summary":"light is on"}))
    );
    assert_eq!(
        fixture.event(&daemon, &pending).await,
        (200, json!({"type":"completed","task_id":pending,"summary":"pending"}))
    );
    fixture.rejected(&expired, "expired task_id").await;
    fixture.rejected(&cancelled, "cancelled task_id").await;
    daemon.stop(true).await;
    let daemon = fixture.start().await;
    for id in [&live, &pending, &expired, &cancelled] {
        assert_eq!(fixture.event(&daemon, id).await, empty());
    }
    fixture.rejected(&live, "task_id already reported").await;
    assert_eq!(fixture.calls().len(), 5);
    assert_eq!(
        std::fs::metadata(fixture.path("tasks.json")).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(std::fs::metadata(fixture.temp.path()).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        std::fs::metadata(fixture.path("report.sock")).unwrap().mode() & 0o777,
        0o600
    );
    daemon.stop(true).await;
}

#[tokio::test]
async fn f11_second_owner_cannot_mutate_and_process_death_releases_lease() {
    let mut fixture = Fixture::new();
    let mut daemon = fixture.start().await;
    let original = fixture.config.clone();
    let before = std::fs::read(fixture.path("tasks.json")).unwrap();
    let inode = std::fs::metadata(fixture.path("tasks.json.lock")).unwrap().ino();
    fixture.config["report_socket"] = json!(fixture.path("second.sock"));
    let output = fixture.command().output().await.unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"store_already_owned\n");
    assert_eq!(std::fs::read(fixture.path("tasks.json")).unwrap(), before);
    assert!(!fixture.path("second.sock").exists());
    let id = fixture.submit(&daemon).await;
    assert_eq!(fixture.event(&daemon, &id).await, progress(&id));
    daemon.child.kill().await.unwrap();
    let output = daemon.child.wait().await.unwrap();
    assert!(!output.success());
    let next = fixture.start().await;
    assert_eq!(std::fs::metadata(fixture.path("tasks.json.lock")).unwrap().ino(), inode);
    assert_eq!(fixture.calls().len(), 1);
    next.stop(true).await;
    fixture.config = original;
}

#[tokio::test]
async fn f12_f16_unsafe_startup_has_no_ready_listener_or_gadget() {
    let mut fixture = Fixture::new();
    let original = fixture.config.clone();
    for change in [
        "certificate",
        "key",
        "corrupt_store",
        "directory_store",
        "missing_parent",
    ] {
        fixture.config = original.clone();
        match change {
            "certificate" => fixture.config["tls_certificate_der"] = json!(fixture.path("absent.der")),
            "key" => {
                crate::service_support::private(&fixture.path("bad.der"), b"bad");
                fixture.config["tls_private_key_der"] = json!(fixture.path("bad.der"));
            }
            "corrupt_store" => crate::service_support::private(&fixture.path("tasks.json"), b"{"),
            "directory_store" => {
                std::fs::remove_file(fixture.path("tasks.json")).unwrap();
                std::fs::create_dir(fixture.path("tasks.json")).unwrap();
            }
            _ => fixture.config["store"] = json!(fixture.path("missing/tasks.json")),
        }
        let output = fixture.command().output().await.unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert!(!fixture.path("report.sock").exists());
        assert_eq!(fixture.calls(), Vec::<Value>::new());
        if change == "corrupt_store" {
            assert_eq!(std::fs::read(fixture.path("tasks.json")).unwrap(), b"{");
        }
    }
}

#[tokio::test]
async fn f18_shutdown_drains_held_child_and_releases_socket_and_store() {
    let fixture = Fixture::new();
    let gate = UnixListener::bind(fixture.path("gate.sock")).unwrap();
    fixture.script(CHAT, "", 0, Some(&fixture.path("gate.sock")));
    let mut daemon = fixture.start().await;
    let (client, url, credential) = (fixture.client.clone(), daemon.url.clone(), fixture.credential.clone());
    let request = tokio::spawn(async move {
        client
            .post(format!("{url}/v1/tasks"))
            .bearer_auth(credential)
            .json(&crate::service_support::task())
            .send()
            .await
            .unwrap()
    });
    let (mut held, _) = gate.accept().await.unwrap();
    assert_eq!(held.read_u8().await.unwrap(), 1);
    daemon.stdin.write_all(b"{\"operation\":\"shutdown\"}\n").await.unwrap();
    let _released = held.write_all(&[1]).await;
    assert_eq!(request.await.unwrap().status(), 201);
    let address = daemon.url.trim_start_matches("https://").to_string();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), daemon.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!fixture.path("report.sock").exists());
    assert!(tokio::net::TcpStream::connect(&address).await.is_err());
    let pid = std::fs::read_to_string(fixture.path("child.pid")).unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    fixture.script(CHAT, "", 0, None);
    let next = fixture.start().await;
    assert_eq!(fixture.calls().len(), 1);
    next.stop(true).await;
}

#[tokio::test]
async fn f18_disconnected_submit_is_supervised_through_child_reaping() {
    let fixture = Fixture::new();
    let gate = UnixListener::bind(fixture.path("disconnect.sock")).unwrap();
    fixture.script(CHAT, "", 0, Some(&fixture.path("disconnect.sock")));
    let daemon = fixture.start().await;
    let (client, url, credential) = (fixture.client.clone(), daemon.url.clone(), fixture.credential.clone());
    let request = tokio::spawn(async move {
        client
            .post(format!("{url}/v1/tasks"))
            .bearer_auth(credential)
            .json(&crate::service_support::task())
            .send()
            .await
    });
    let (mut held, _) = gate.accept().await.unwrap();
    assert_eq!(held.read_u8().await.unwrap(), 1);
    request.abort();
    let _cancelled = request.await;
    let pid = std::fs::read_to_string(fixture.path("child.pid")).unwrap();
    // Leave the fake held. Production's child deadline must kill AND wait;
    // shutdown must keep the store leased until that supervision completes.
    daemon.stop(true).await;
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(fixture.calls().len(), 1);
    assert!(!fixture.path("report.sock").exists());
}

#[tokio::test]
async fn f19_child_capacity_rejects_without_extra_admission() {
    let fixture = Fixture::new();
    let gate = UnixListener::bind(fixture.path("capacity.sock")).unwrap();
    fixture.script(CHAT, "", 0, Some(&fixture.path("capacity.sock")));
    let daemon = fixture.start().await;
    let mut requests = tokio::task::JoinSet::new();
    let mut held = Vec::new();
    for _ in 0..4 {
        let (client, url, credential) = (fixture.client.clone(), daemon.url.clone(), fixture.credential.clone());
        requests.spawn(async move {
            client
                .post(format!("{url}/v1/tasks"))
                .bearer_auth(credential)
                .json(&crate::service_support::task())
                .send()
                .await
                .unwrap()
        });
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(2), gate.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), 1);
        held.push(stream);
    }
    assert_eq!(
        fixture
            .request(&daemon, "POST", "/v1/tasks", Some(crate::service_support::task()))
            .await,
        (503, json!({"error":"service_busy"}))
    );
    assert_eq!(fixture.calls().len(), 4);
    for stream in &mut held {
        stream.write_all(&[1]).await.unwrap();
    }
    while let Some(response) = requests.join_next().await {
        assert_eq!(response.unwrap().status(), 201);
    }
    daemon.stop(true).await;
}

#[tokio::test]
async fn f18_sigterm_cleans_socket_and_retains_lock_inode() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start().await;
    let pid = daemon.child.id().unwrap();
    let status = Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(status.success());
    assert!(
        tokio::time::timeout(Duration::from_secs(8), daemon.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!fixture.path("report.sock").exists());
    assert!(fixture.path("tasks.json.lock").exists());
    assert_eq!(DAEMON, env!("CARGO_BIN_EXE_echo-muse-daemon"));
}

#[tokio::test]
async fn f09_single_live_task_reopens_twice_with_one_send_and_no_consumed_replay() {
    let fixture = Fixture::new();
    let daemon = fixture.start().await;
    let id = fixture.submit(&daemon).await;
    assert_eq!(fixture.event(&daemon, &id).await, progress(&id));
    let original = fixture.calls()[0]["argv"][2].clone();
    daemon.stop(true).await;
    let daemon = fixture.start().await;
    fixture.accepted(&id, "completed", "light is on").await;
    assert_eq!(
        fixture.event(&daemon, &id).await,
        (200, json!({"type":"completed","task_id":id,"summary":"light is on"}))
    );
    daemon.stop(true).await;
    let daemon = fixture.start().await;
    assert_eq!(fixture.event(&daemon, &id).await, empty());
    fixture.rejected(&id, "task_id already reported").await;
    assert_eq!(fixture.calls().len(), 1);
    assert_eq!(fixture.calls()[0]["argv"][2], original);
    daemon.stop(true).await;
}
