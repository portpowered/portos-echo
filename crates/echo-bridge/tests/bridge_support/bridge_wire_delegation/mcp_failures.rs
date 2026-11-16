//! T3: uncertain external mutations never publish or automatically replay a grant.
use super::*;

const STORAGE: &str = "Grant storage failed; rotation may require recovery";
const TRANSPORT: &str = "OAuth transport failed; operation may be incomplete";

#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_failed_rotation_storage_or_transport_blocks_use_and_replay() {
    drive(async {
        for storage in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let mock = Mock::bind().await.unwrap();
            let mut session = Session::start(directory.path(), &mock, true).await;
            journey::link(&mut session, &mock, directory.path()).await;
            let original = record(directory.path());
            let path = directory.path().join("portos/grant.json");
            let retained = directory.path().join("retained.private.json");
            let message = if storage {
                std::fs::rename(&path, &retained).unwrap();
                std::fs::create_dir(&path).unwrap();
                STORAGE
            } else {
                mock.state.lock().unwrap().fault = Some(("/custom/token".into(), 503, json!({"private":"do not print"})));
                TRANSPORT
            };
            completed(&operator(directory.path(), "--refresh-now").await.unwrap(), 1, "", &format!("echo-bridge: {message}\n"));
            assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"error","message":message,"relink":true}));
            if storage {
                assert_eq!(mock.state.lock().unwrap().generation, 2);
                // Repair only the fixture's filesystem obstacle. This must not
                // clear the running owner's uncertain mutation latch.
                std::fs::remove_dir(&path).unwrap();
                std::fs::rename(retained, &path).unwrap();
            }
            assert!(record(directory.path()) == original, "uncommitted grant replaced durable state");
            completed(&operator(directory.path(), "--refresh-now").await.unwrap(), 1, "", "echo-bridge: Grant requires relinking\n");
            assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true}));
            session.process.finish(0).await;
            assert_uncertain(&mock, directory.path());
        }
    }).await;
}

#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_cancelled_rotated_response_never_replays_or_publishes() {
    drive(async {
        let directory = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        let mut session = Session::start(directory.path(), &mock, true).await;
        journey::link(&mut session, &mock, directory.path()).await;
        let original = record(directory.path());
        mock.state.lock().unwrap().hold = Some("/custom/token".into());
        let url = format!("http://{}/operator/mcp", session.control);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let request = tokio::spawn(client.post(url).bearer_auth("synthetic-screen-pair").json(&json!({"operation":"refresh"})).send());
        while !mock.state.lock().unwrap().held { tokio::task::yield_now().await; }
        assert_eq!(mock.state.lock().unwrap().generation, 2);
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true}));
        mock.release.notify_one();
        completed(&operator(directory.path(), "--refresh-now").await.unwrap(), 1, "", "echo-bridge: Grant requires relinking\n");
        assert!(record(directory.path()) == original, "cancelled mutation published grant");
        session.process.finish(0).await;
        assert_uncertain(&mock, directory.path());
    }).await;
}

fn assert_uncertain(mock: &Mock, directory: &Path) {
    let state = mock.state.lock().unwrap();
    assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/link").count(), 1);
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|r| r.form.get("grant_type").is_some_and(|v| v == "refresh_token"))
            .count(),
        1
    );
    assert_eq!(state.requests.iter().filter(|r| r.path == "/mcp").count(), 0);
    let events = trace(&directory.join("trace"));
    assert_eq!(
        events.iter().filter(|e| e["event"] == "mcp.refresh.committed").count(),
        0
    );
    let text = std::fs::read_to_string(directory.join("trace")).unwrap();
    for private in ["do not print", "refresh-1", "access-2", "private_key_jwk"] {
        assert!(!text.contains(private));
    }
}
