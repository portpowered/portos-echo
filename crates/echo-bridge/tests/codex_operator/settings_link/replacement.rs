//! Port OS grant checks against the current screen of a delivered bridge.
use super::*;

pub(in crate::codex_operator) async fn replacement_grant(
    fixture: &Fixture,
    address: SocketAddr,
    mock: &echo_lab::lab_mcp::Fixture,
    epoch: u64,
) {
    tokio::select! {
        biased;
        () = follow_subprocess_clock(mock, epoch) => unreachable!("the fixture clock runs until the grant journey ends"),
        () = replacement_journey(fixture, address, mock, async |expires| {
            // The normal process takes its clock origin after the mock was started.
            let now = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
            assert!((epoch + 1800..=now + 1800).contains(&expires));
        }) => {}
    }
}

pub(in crate::codex_operator) async fn controlled_grant(
    fixture: &Fixture,
    address: SocketAddr,
    mock: &echo_lab::lab_mcp::Fixture,
    epoch: u64,
    process: &mut Process,
) {
    replacement_journey(fixture, address, mock, async |expires| {
        assert_eq!(expires, epoch + 1800);
        mock.advance(5).unwrap();
        process
            .input
            .as_mut()
            .unwrap()
            .write_all(b"{\"action\":\"advance_clock\",\"milliseconds\":5000}\n")
            .await
            .unwrap();
        assert_eq!(
            process.line().await,
            r#"{"event":"bridge.clock_advanced","milliseconds":5000}"#
        );
    })
    .await;
}

async fn follow_subprocess_clock(mock: &echo_lab::lab_mcp::Fixture, mut epoch: u64) {
    // Align the mock's DPoP validator with the normal subprocess clock.
    let mut ticks = tokio::time::interval(Duration::from_millis(50));
    loop {
        ticks.tick().await;
        let now = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
        if now > epoch {
            mock.advance(now - epoch).unwrap();
            epoch = now;
        }
    }
}

async fn replacement_journey(
    fixture: &Fixture,
    address: SocketAddr,
    mock: &echo_lab::lab_mcp::Fixture,
    observe_expiry: impl AsyncFnOnce(u64),
) {
    let mut journey = PairedGrant {
        fixture,
        address,
        mock,
        client: reqwest::Client::new(),
        captured: vec![],
    };
    let status = journey.portos().await;
    assert_eq!(status["link_status"], "link_required");
    assert_eq!(status["link_waiting_for_restart"], false);
    assert_eq!(status["server"]["requested_scopes"], json!(["endpoint:read"]));
    let expires = journey.begin().await;
    mock.approve(echo_lab::lab_mcp::Approval::Approved).unwrap();
    observe_expiry(expires).await;
    wait_for_link(&mut journey).await;
    unlink_paired_grant(&mut journey).await;
    let grant: Value =
        serde_json::from_slice(&std::fs::read(fixture.directory.path().join("grant/grant.json")).unwrap()).unwrap();
    assert_eq!(grant["grant"], Value::Null);
}
