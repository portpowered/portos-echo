//! T3: public admission/status, real serial installer and local HTTPS fixture.
use super::*;
use crate::support::TOKEN;
use echo_contracts::secret::Secret;
use echo_screen_server::{SettingsOperation, SettingsReply};
use serde_json::{Value, json};
use std::net::SocketAddr;

async fn mounted(home: &Path, ca: Option<std::path::PathBuf>) -> Result<echo_screen_server::ScreenServer> {
    let (send, receive) = tokio::sync::mpsc::channel(1);
    let (host, mut inbox) = echo_screen_server::settings_channel();
    let mut server =
        echo_screen_server::serve_with_settings("127.0.0.1:0".parse()?, Secret::new(TOKEN.into()), receive, Some(host))
            .await?;
    tokio::spawn(async move {
        let _states = send;
        while let Some(request) = inbox.recv().await {
            request
                .commit(|operation| match operation {
                    SettingsOperation::PackCatalog(catalog) => SettingsReply::Packs(catalog.with_selection(None)),
                    _ => SettingsReply::InvalidPackSelection,
                })
                .await;
        }
    });
    let mut configured = server.install_configured_packs(home.into(), ca, vec![]);
    assert!(configured.recv().await.is_none());
    Ok(server)
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
        request(addr, "POST", &path, &good, "Origin: http://foreign.invalid\r\n")
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
