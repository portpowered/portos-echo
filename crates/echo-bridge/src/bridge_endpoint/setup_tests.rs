use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct DelayedRoute(AtomicBool);

impl RouteResolver for DelayedRoute {
    fn resolve<'a>(&'a self, auth_link_id: &'a str, tokens: &'a dyn EndpointTokenSource) -> Resolve<'a> {
        if self.0.load(Ordering::Acquire) {
            Resolver.resolve(auth_link_id, tokens)
        } else {
            Box::pin(async { None })
        }
    }
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_resumes_route_lookup_without_a_new_exchange() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let resolver = Arc::new(DelayedRoute::default());
        let settings =
            Settings::with_resolver(home.path(), &mock, &gateway, grant.grant.clone(), resolver.clone()).await;
        settings.save(true, "Hall Echo").await;
        wait_approval(&settings).await;
        advance(Duration::from_secs(5)).await;
        wait_unavailable(&settings).await;
        let retained = read_install(home.path());
        assert_eq!(
            retained["installation"],
            json!({"authLinkId":LINK, "deviceName":"Hall Echo"})
        );
        assert!(gateway.received.try_recv().is_err());

        settings.save(false, "Hall Echo").await;
        resolver.0.store(true, Ordering::Release);
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(2).await;
        assert_installed(home.path(), &mock).await;
        let install_requests = mock
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.path == "/plugin-auth-code-exchanges")
            .count();
        assert_eq!(install_requests, 1);
        settings.close().await;
        grant.close().await;
    })
    .await;
}

async fn wait_unavailable(settings: &Settings) {
    for _ in 0..50 {
        if settings.snapshot().await["warnings"] == json!([UNAVAILABLE]) {
            return;
        }
    }
    panic!("endpoint route refusal was not shown in Settings");
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_without_a_production_resolver_keeps_settings_and_mcp_available() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let (owner, _) = settings_owner(home.path(), &mock);
        let (service, send) = SettingsService::resume(Box::new(owner));
        let settings = Settings::serve(service, send, grant.grant.clone()).await;
        settings.save(true, "Kitchen").await;
        let view = settings.snapshot().await;
        assert_eq!(view["portos"]["endpoint_name"], "Kitchen");
        assert_eq!(view["portos"]["endpoint_enabled"], true);
        assert_eq!(view["portos"]["link_status"], "linked");
        assert_eq!(view["warnings"], json!([UNAVAILABLE]));
        advance(Duration::from_secs(61)).await;
        assert_eq!(settings.snapshot().await["portos"]["challenge"], Value::Null);
        assert_eq!(
            mock.state
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|request| {
                    request.path == "/custom/link"
                        || request.path == "/plugin-auth-code-exchanges"
                        || request.path == "/messages"
                })
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            ["/custom/link"]
        );
        assert!(!home.path().join("portos-endpoint").exists());
        settings.close().await;
        grant.close().await;
    })
    .await;
}
