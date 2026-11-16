//! T3 partial-startup fault at the Live factory boundary, with real acquired
//! host/Spoof handles. No fake substitutes for the resources being released.
use super::world::World;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
#[ignore = "T3 requires prebuilt pinned hassmic-host"]
async fn t3_failed_live_start_releases_the_already_running_host_and_spoof() {
    let root = std::env::temp_dir().join(format!("lab-partial-start-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut world = World::default();
    let result = world
        .start_with(python, std::path::Path::new("not-launched"), &root, async {
            Err(echo_live::call::CallError::Signaling)
        })
        .await;
    assert_eq!(result, Err("launch: fake Live startup failed".into()));
    assert_eq!(world.endpoints.len(), 2);
    for endpoint in &world.endpoints {
        let connection = tokio::net::TcpStream::connect(endpoint).await.unwrap();
        assert_eq!(connection.peer_addr().unwrap(), *endpoint);
        assert_eq!(endpoint.ip(), std::net::Ipv4Addr::LOCALHOST);
    }
    assert_eq!(world.close().await, Ok(()));
    assert_eq!(world.close().await, Ok(()));
    assert!(tokio::net::TcpStream::connect(world.endpoints[1]).await.is_err());
    if let Ok(mut stream) = tokio::net::TcpStream::connect(world.endpoints[0]).await {
        assert!(cfg!(windows) && stream.peer_addr().unwrap() == world.endpoints[0]);
        let _ = stream.write_all(b"{\"type\":\"describe\"}\n").await;
        let response = tokio::time::timeout(std::time::Duration::from_secs(1), stream.read_u8())
            .await
            .unwrap();
        assert!(response.is_err());
    }
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir(root).unwrap();
}
