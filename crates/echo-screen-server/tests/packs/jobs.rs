//! T3: public admission/status, real serial installer and local HTTPS fixture.
use super::*;
use crate::support::TOKEN;
use echo_contracts::secret::Secret;
use echo_screen_server::{SettingsOperation, SettingsReply};
use serde_json::{Value, json};
use std::net::SocketAddr;

async fn mounted(home: &Path, ca: Option<std::path::PathBuf>) -> Result<echo_screen_server::ScreenServer> {
    mounted_selection(home, ca, None).await
}

async fn mounted_selection(
    home: &Path,
    ca: Option<std::path::PathBuf>,
    selection: Option<String>,
) -> Result<echo_screen_server::ScreenServer> {
    let mut server = hosted(selection).await?;
    let mut configured = server.install_configured_packs(home.into(), ca, vec![]);
    assert!(configured.recv().await.is_none());
    Ok(server)
}

async fn hosted(selection: Option<String>) -> Result<echo_screen_server::ScreenServer> {
    let (send, receive) = tokio::sync::mpsc::channel(1);
    let (host, mut inbox) = echo_screen_server::settings_channel();
    let server =
        echo_screen_server::serve_with_settings("127.0.0.1:0".parse()?, Secret::new(TOKEN.into()), receive, Some(host))
            .await?;
    tokio::spawn(async move {
        let _states = send;
        while let Some(request) = inbox.recv().await {
            request
                .commit(|operation| match operation {
                    SettingsOperation::PackCatalog(catalog) => {
                        SettingsReply::Packs(catalog.with_selection(selection.as_deref()))
                    }
                    _ => SettingsReply::InvalidPackSelection,
                })
                .await;
        }
    });
    Ok(server)
}

#[tokio::test]
async fn t3_screen_pack_configured_completion_notifies_before_result_and_skips_duplicate() -> Result {
    use tokio::io::AsyncReadExt;
    let peer = Peer::start().await?;
    let home = tempfile::tempdir()?;
    let mut server = hosted(None).await?;
    let mut events = crate::support::events(server.bound_addr).await?;
    let pin = (format!("{}/good", peer.base), peer.digest());
    let mut results = server.install_configured_packs(home.path().into(), Some(peer.ca()), vec![pin.clone(), pin]);
    for index in 0..2 {
        let result = results.recv().await.ok_or("missing configured result")?;
        assert_eq!(result.installed.as_deref(), Some("template@0.1.0"));
        assert_eq!(result.reason, None);
        assert_eq!(catalog(server.bound_addr).await?["revision"], 1);
        if index == 0 {
            reload(&mut events, json!("template@0.1.0")).await?;
        }
    }
    assert!(results.recv().await.is_none());
    server.revoke_pairing().await?;
    let mut wire = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(2), events.read_to_string(&mut wire)).await??;
    assert_eq!(wire.matches("event: screen-pack-reload").count(), 0);
    assert!(!home.path().join("settings.toml").exists());
    server.shutdown().await?;
    peer.stop().await
}

#[tokio::test]
async fn t3_screen_pack_install_reload_once_uses_accepted_selection_and_deduplicates() -> Result {
    use tokio::io::AsyncReadExt;
    let peer = Peer::start().await?;
    for selection in [None, Some(String::new())] {
        let home = tempfile::tempdir()?;
        let mut server = mounted_selection(home.path(), Some(peer.ca()), selection.clone()).await?;
        let addr = server.bound_addr;
        let mut events = crate::support::events(addr).await?;
        assert_eq!(catalog(addr).await?["revision"], 0);
        let url = format!("{}/good", peer.base);
        assert_eq!(post(addr, &url, &peer.digest()).await?.0, 202);
        let installed = settled(addr, "1", "installed").await?;
        let active = if selection.is_none() {
            json!("template@0.1.0")
        } else {
            Value::Null
        };
        assert_eq!(installed["active"], active);
        assert_eq!(installed["revision"], 1);
        reload(&mut events, active).await?;
        assert_eq!(post(addr, &url, &peer.digest()).await?.0, 202);
        assert_eq!(settled(addr, "2", "installed").await?["revision"], 1);
        assert_eq!(post(addr, &url, &"0".repeat(64)).await?.0, 202);
        assert_eq!(settled(addr, "3", "failed").await?["revision"], 1);
        server.revoke_pairing().await?;
        let mut wire = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(2), events.read_to_string(&mut wire)).await??;
        assert_eq!(wire.matches("event: screen-pack-reload").count(), 0);
        assert!(!home.path().join("settings.toml").exists());
        server.shutdown().await?;
    }
    peer.stop().await
}

async fn reload(events: &mut BufReader<tokio::net::TcpStream>, active: Value) -> Result {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut named = false;
        loop {
            let mut line = String::new();
            if events.read_line(&mut line).await? == 0 {
                return Err("missing reload".into());
            }
            if line.starts_with("event: ") {
                assert_eq!(line.trim(), "event: screen-pack-reload");
                assert!(!named);
                named = true;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                assert!(named);
                assert_eq!(
                    serde_json::from_str::<Value>(data)?,
                    json!({"version":1,"revision":1,"active":active})
                );
                return Ok(());
            }
        }
    })
    .await?
}

async fn post(addr: SocketAddr, url: &str, sha256: &str) -> Result<(u16, Value)> {
    let body = json!({"version":1,"url":url,"sha256":sha256}).to_string();
    let (status, body) = request(
        addr,
        "POST",
        &format!("/screen-packs/v1/install?token={TOKEN}"),
        &body,
        "Content-Type: application/json\r\n",
    )
    .await?;
    Ok((
        status,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&body)?
        },
    ))
}

async fn catalog(addr: SocketAddr) -> Result<Value> {
    let (status, body) = request(addr, "GET", &format!("/screen-packs/v1?token={TOKEN}"), "", "").await?;
    assert_eq!(status, 200);
    Ok(serde_json::from_str(&body)?)
}

async fn settled(addr: SocketAddr, id: &str, status: &str) -> Result<Value> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let value = catalog(addr).await?;
            if value["jobs"]
                .as_array()
                .ok_or("missing jobs")?
                .iter()
                .any(|j| j["id"] == id && j["status"] == status)
            {
                return Ok(value);
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}

#[tokio::test]
async fn t3_screen_pack_interactive_install_status_checksum_failure_and_continued_serving() -> Result {
    let peer = Peer::start().await?;
    let home = tempfile::tempdir()?;
    let mut server = mounted(home.path(), Some(peer.ca())).await?;
    let addr = server.bound_addr;
    let (code, admitted) = post(addr, &format!("{}/good", peer.base), &peer.digest().to_uppercase()).await?;
    assert_eq!((code, admitted), (202, json!({"version":1,"job":"1"})));
    let good = settled(addr, "1", "installed").await?;
    assert_eq!(good["active"], "template@0.1.0");
    assert_eq!(good["jobs"], json!([{"id":"1","status":"installed","reason":null}]));
    assert_eq!(
        post(addr, &format!("{}/good", peer.base), &"0".repeat(64)).await?,
        (202, json!({"version":1,"job":"2"}))
    );
    let bad = settled(addr, "2", "failed").await?;
    assert_eq!(bad["active"], "template@0.1.0");
    assert_eq!(
        bad["jobs"][1],
        json!({"id":"2","status":"failed","reason":"checksum_mismatch"})
    );
    assert_eq!(
        std::fs::read(home.path().join("screens/template@0.1.0/rest.png"))?,
        b"synthetic-rest"
    );
    assert_eq!(request(addr, "GET", "/screen.css", "", "").await?.0, 200);
    assert_eq!(std::fs::read_dir(home.path().join("screens"))?.count(), 2);
    assert!(!home.path().join("settings.toml").exists());
    assert!(!bad.to_string().contains(&peer.base));
    assert!(!bad.to_string().contains(TOKEN));
    server.shutdown().await?;
    peer.stop().await
}

#[tokio::test]
async fn t3_screen_pack_interactive_auth_origin_media_and_revocation() -> Result {
    let home = tempfile::tempdir()?;
    let mut server = mounted(home.path(), None).await?;
    let addr = server.bound_addr;
    let url = "https://127.0.0.1/pack";
    let good = json!({"version":1,"url":url,"sha256":"a".repeat(64)}).to_string();
    let path = format!("/screen-packs/v1/install?token={TOKEN}");
    for query in [
        "",
        "?token=",
        "?token=wrong",
        "?token=synthetic-screen-pair&token=synthetic-screen-pair",
    ] {
        assert_eq!(
            request(addr, "POST", &format!("/screen-packs/v1/install{query}"), "{", "").await?,
            (401, String::new())
        );
    }
    assert_eq!(
        request(addr, "POST", &path, &good, "Origin: http://127.0.0.1:1\r\n")
            .await?
            .0,
        403
    );
    assert_eq!(request(addr, "POST", &path, &good, "").await?.0, 415);
    assert_eq!(catalog(addr).await?["jobs"], json!([]));
    server.revoke_pairing().await?;
    assert_eq!(post(addr, url, &"a".repeat(64)).await?, (401, Value::Null));
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_screen_pack_interactive_shape_and_safe_errors() -> Result {
    let home = tempfile::tempdir()?;
    let mut server = mounted(home.path(), None).await?;
    let addr = server.bound_addr;
    let url = "https://127.0.0.1/pack";
    let good = json!({"version":1,"url":url,"sha256":"a".repeat(64)}).to_string();
    let path = format!("/screen-packs/v1/install?token={TOKEN}");
    for body in [
        "{".to_owned(),
        good.replace("\"version\":1", "\"version\":2"),
        good.replace("\"version\":1", "\"extra\":1,\"version\":1"),
    ] {
        assert_eq!(
            request(addr, "POST", &path, &body, "Content-Type: application/json\r\n")
                .await?
                .0,
            400
        );
    }
    assert_eq!(
        request(
            addr,
            "POST",
            &path,
            &" ".repeat(65_537),
            "Content-Type: application/json\r\n"
        )
        .await?
        .0,
        413
    );
    assert_eq!(
        post(addr, "http://127.0.0.1/pack", &"a".repeat(64)).await?,
        (422, json!({"version":1,"reason":"invalid_url"}))
    );
    assert_eq!(
        post(addr, url, "bad").await?,
        (422, json!({"version":1,"reason":"invalid_sha256"}))
    );
    assert_eq!(catalog(addr).await?["jobs"], json!([]));
    assert_eq!(post(addr, url, &"a".repeat(64)).await?.0, 202);
    assert_eq!(
        settled(addr, "1", "failed").await?["jobs"],
        json!([{"id":"1","status":"failed","reason":"tls_failed"}])
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn t3_screen_pack_interactive_queue_bound_serial_worker_and_shutdown_cleanup() -> Result {
    let mut peer = Peer::start().await?;
    let home = tempfile::tempdir()?;
    let mut server = mounted(home.path(), Some(peer.ca())).await?;
    let addr = server.bound_addr;
    assert_eq!(
        post(addr, &format!("{}/stall", peer.base), &peer.digest()).await?.0,
        202
    );
    let mut line = String::new();
    peer.output.read_line(&mut line).await?;
    assert_eq!(line.trim(), "stalled");
    for id in 2..=4 {
        assert_eq!(
            post(addr, &format!("{}/good", peer.base), &peer.digest()).await?,
            (202, json!({"version":1,"job":id.to_string()}))
        );
    }
    assert_eq!(
        post(addr, &format!("{}/good", peer.base), &peer.digest()).await?,
        (503, Value::Null)
    );
    assert_eq!(
        catalog(addr).await?["jobs"],
        json!([
            {"id":"1","status":"downloading","reason":null},
            {"id":"2","status":"queued","reason":null},
            {"id":"3","status":"queued","reason":null},
            {"id":"4","status":"queued","reason":null}
        ])
    );
    server.shutdown().await?;
    assert_eq!(std::fs::read_dir(home.path().join("screens"))?.count(), 0);
    peer.stop().await
}
