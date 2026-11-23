use crate::support::{Result, TOKEN, report, request};
use echo_contracts::secret::Secret;
use echo_screen_server::{WifiRelay, serve_with_wifi};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

const SCAN: &str = r#"{"type":"wifi-request","data":{"version":1,"op":"scan"}}"#;
const EMPTY: &str = r#"{"type":"wifi-result","data":{"version":1,"op":"scan","ok":true,"networks":[]}}"#;

#[tokio::test]
async fn t3_wifi_relay_checks_pairing_origin_and_body_before_provider() -> Result {
    let calls = Arc::new(AtomicUsize::new(0));
    let relay = WifiRelay::new({
        let calls = calls.clone();
        move |body| {
            let calls = calls.clone();
            async move {
                if body != SCAN.as_bytes() {
                    return None;
                }
                calls.fetch_add(1, Ordering::Relaxed);
                Some(EMPTY.as_bytes().to_vec())
            }
        }
    });
    let (_tx, rx) = mpsc::channel(1);
    let mut server = serve_with_wifi(
        "127.0.0.1:0".parse()?,
        Secret::new(TOKEN.into()),
        rx,
        None,
        false,
        Some(relay),
    )
    .await?;
    let addr = server.bound_addr;
    let base = "http://127.0.0.1";
    let origin = format!("{base}:{}", addr.port());
    let correct = format!("Origin: {origin}\r\nContent-Type: application/json\r\n");
    let path = format!("/_screen/wifi?token={TOKEN}");
    let over_limit = " ".repeat(1025);
    for (uri, body, headers, expected) in [
        ("/_screen/wifi?token=incorrect", SCAN, correct.as_str(), 401),
        (&path, SCAN, "Content-Type: application/json\r\n", 403),
        (
            &path,
            SCAN,
            "Origin: http://127.0.0.1:1\r\nContent-Type: application/json\r\n",
            403,
        ),
        (
            &path,
            SCAN,
            &format!("Origin: {origin}\r\nContent-Type: text/plain\r\n"),
            415,
        ),
        (&path, &over_limit, &correct, 413),
        (&path, "{}", &correct, 400),
    ] {
        assert_eq!(
            request(addr, "POST", uri, body, headers).await?,
            (expected, String::new())
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(request(addr, "POST", &path, SCAN, &correct).await?, (200, EMPTY.into()));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    report("Wi-Fi relay: 401/403/415/413/400 refusals made 0 provider calls; paired scan returned 200 and []");
    server.shutdown().await?;
    Ok(())
}
