//! T3: grant scopes survive the actual link, rotation and private-store reload.
use super::*;

#[tokio::test(start_paused = true)]
async fn t3_portos_full_and_narrow_grants_preserve_scopes_across_rotation_and_reload() {
    for scopes in [
        echo_config::McpConfig::default().scopes,
        vec!["endpoint:read".into()],
        vec!["subscription:read".into(), "subscription:write".into()],
    ] {
        let h = Harness::with_scopes(scopes.clone()).await;
        h.link().await;
        assert_eq!(h.fixture.shared.lock().unwrap().scopes, scopes.join(" "));
        for refresh in [false, true] {
            if refresh {
                drive(h.client.force_refresh()).await.unwrap();
            }
            let token = drive(h.client.access_token()).await.unwrap();
            let (_, claims, _, _) = mcp_proof::decode(token.expose()).unwrap();
            assert_eq!(claims["scope"], scopes.join(" "));
            assert_eq!(h.request(token.expose(), "initialize").await.status(), 200);
            assert_eq!(h.record()["grant"]["scope"], scopes.join(" "));
        }
        let mut options = h.options();
        options.scopes = scopes.clone();
        let reloaded = drive(AuthClient::open_at_epoch(options, 1_800_000_005)).await.unwrap();
        let token = drive(reloaded.access_token()).await.unwrap();
        let (_, claims, _, _) = mcp_proof::decode(token.expose()).unwrap();
        assert_eq!(claims["scope"], scopes.join(" "));
        assert_eq!(h.request(token.expose(), "initialize").await.status(), 200);
        assert_eq!(h.fixture.counts().unwrap().refreshes, 1);
        drop(reloaded);
        h.close().await;
    }
}
