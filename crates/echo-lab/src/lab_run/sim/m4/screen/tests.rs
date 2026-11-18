//! T3 observer faults at the SSE socket boundary; no hardware or provider.
use super::*;
use tokio::net::TcpListener;

#[tokio::test]
async fn missing_listener_cannot_reuse_another_world_screen() {
    let root = std::env::temp_dir().join(format!("m4-no-screen-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    assert_eq!(Screen::connect(&root).await.err(), Some("screen: missing trace".into()));
    std::fs::write(root.join("trace"), "{\"event\":\"bridge.ready\"}\n").unwrap();
    assert_eq!(
        Screen::connect(&root).await.err(),
        Some("screen: missing listener".into())
    );
    std::fs::remove_dir_all(root).unwrap();
}

async fn response(bytes: &'static [u8]) -> (Screen, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(bytes).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    (Screen(TcpStream::connect(address).await.unwrap()), task)
}

#[tokio::test]
async fn t3_final_collection_retains_late_mouth_publications_and_rejects_truncation() {
    let (mut screen, task) = response(
        b"data: {\"seq\":5,\"state\":{\"state\":\"ready\"}}\n\ndata: {\"seq\":6,\"state\":{\"state\":\"speaking\"}}\n\ndata: {\"seq\":7,\"state\":{\"state\":\"ready\"}}\n\n",
    )
    .await;
    screen.expect("ready").await.unwrap();
    assert_eq!(
        screen.remaining().await.unwrap(),
        vec![
            serde_json::json!({"seq":6,"state":{"state":"speaking"}}),
            serde_json::json!({"seq":7,"state":{"state":"ready"}}),
        ]
    );
    task.await.unwrap();
    let (mut screen, task) = response(b"data: {\"state\":{\"state\":\"ready\"}}\n\n").await;
    screen.expect("ready").await.unwrap();
    assert_eq!(screen.remaining().await.unwrap(), Vec::<Value>::new());
    task.await.unwrap();
    let (screen, task) = response(b"data: {\"seq\":6}").await;
    assert_eq!(
        screen.remaining().await,
        Err("screen: incomplete or oversized final publication".into())
    );
    task.await.unwrap();
}

#[tokio::test]
#[ignore = "T3 loopback SSE observer"]
async fn t3_screen_missing_stale_malformed_and_eof_never_supply_speaking() {
    for (bytes, expected) in [
        (b"".as_slice(), "screen: EOF before observation"),
        (
            b"data: {\"state\":{\"state\":\"ready\"}}\n",
            "screen: expected current state speaking, observed \"ready\"",
        ),
        (b"data: {}\n", "screen: expected current state speaking, observed null"),
        (b"data: invalid\n", "screen: malformed state"),
    ] {
        let (mut screen, task) = response(bytes).await;
        assert_eq!(screen.expect("speaking").await, Err(expected.into()));
        task.await.unwrap();
    }
    let (mut screen, task) = response(b"data: {\"state\":{\"state\":\"speaking\"}}\n").await;
    assert_eq!(
        screen.expect("speaking").await,
        Ok(serde_json::json!({"state":"speaking"}))
    );
    // The previous observation cannot be borrowed for a later expectation.
    assert_eq!(
        screen.expect("speaking").await,
        Err("screen: EOF before observation".into())
    );
    task.await.unwrap();
}

#[tokio::test]
#[ignore = "T3 loopback SSE handshake"]
async fn t3_screen_connect_rejects_unauthorized_and_truncated_handshakes() {
    for reply in [b"HTTP/1.1 401 Unauthorized\r\n\r\n".as_slice(), b""] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let root = std::env::temp_dir().join(format!("m4-screen-{}-{}", std::process::id(), address.port()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("trace"),
            serde_json::json!({"event":"screen.listening","address":address.to_string()}).to_string(),
        )
        .unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            assert_eq!(
                request,
                b"GET /events?token=test-pair HTTP/1.0\r\nHost: local\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(reply).await.unwrap();
            // End the response while retaining our socket to observe the client's cleanup.
            socket.shutdown().await.unwrap();
            // Closing with unread response bytes may reset TCP instead of sending FIN.
            assert!(matches!(
                socket.read_u8().await.unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
            ));
            drop(socket);
            drop(listener);
        });
        let error = Screen::connect(&root).await.err().unwrap();
        assert_eq!(
            error,
            if reply.is_empty() {
                "screen: EOF before observation"
            } else {
                "screen: paired SSE unauthorized"
            }
        );
        task.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn t3_final_collection_decodes_real_http_sse_through_eof() {
    use axum::{Router, routing::get};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("trace"),
        serde_json::json!({"event":"screen.listening","address":address.to_string()}).to_string(),
    )
    .unwrap();
    let app = Router::new().route("/events", get(|| async {
        // HTTP/1.1 streams use chunk framing, including a terminal zero chunk.
        // HTTP/1.0 streams use EOF framing, as requested by this observer.
        axum::response::Response::new(axum::body::Body::from_stream(axum::body::Body::from("data: {\"seq\":5,\"state\":{\"state\":\"ready\"}}\n\ndata: {\"seq\":6,\"state\":{\"state\":\"speaking\"}}\n\ndata: {\"seq\":7,\"state\":{\"state\":\"ready\"}}\n\n").into_data_stream()))
    }));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut screen = Screen::connect(root.path()).await.unwrap();
    screen.expect("ready").await.unwrap();
    assert_eq!(
        screen.remaining().await.unwrap(),
        vec![
            serde_json::json!({"seq":6,"state":{"state":"speaking"}}),
            serde_json::json!({"seq":7,"state":{"state":"ready"}}),
        ]
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
