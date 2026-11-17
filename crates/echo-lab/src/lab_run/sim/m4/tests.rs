//! T3 probe ordering and full-golden negative comparison; journeys use `codex::tests`.
use echo_contracts::wyoming::{Decoder, Event, Header, encode};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn send(peer: &mut TcpStream, name: &str, data: Value) {
    let event = Event {
        header: Header {
            r#type: name.into(),
            data: Some(data),
            data_length: 0,
            payload_length: 0,
        },
        data: None,
        payload: vec![],
    };
    peer.write_all(&encode(&event)).await.unwrap();
}

async fn receive(peer: &mut TcpStream, decoder: &mut Decoder) -> Event {
    loop {
        let byte = peer.read_u8().await.unwrap();
        if let Some(value) = decoder.feed(&[byte]).unwrap().pop() {
            return value;
        }
    }
}
pub(super) fn assert_recorded_comparison(raw: &str) {
    let mut actual = crate::lab_run::sim::trace::Trace::default();
    actual.append(format!("{raw}\n").as_bytes()).unwrap();
    let recorded = format!("{}\n", super::golden::normalize(&actual).unwrap().join("\n"));
    assert_eq!(super::golden::compare(&actual, recorded.as_bytes()), Ok(()));
    let index = actual.records.iter().position(|r| r["event"] == "tools/call").unwrap();
    let name = actual.records[index]["name"].as_str().unwrap().to_owned();
    actual.records[index]["name"] = json!("test.wait");
    let error = super::golden::compare(&actual, recorded.as_bytes()).unwrap_err();
    assert!(error.contains(&format!("line {}\n- ", index + 1)), "{error}");
    assert!(error.contains(&format!("\"name\":\"{name}\"")), "{error}");
    assert!(error.contains("\"name\":\"test.wait\""), "{error}");
}

async fn receive_playback(peer: &mut TcpStream, decoder: &mut Decoder) -> Event {
    loop {
        let event = receive(peer, decoder).await;
        if event.header.r#type != "describe" {
            return event;
        }
        // An idle health probe can already be queued when the wake arrives.
        // Serve control traffic without discarding or reordering reply frames.
        assert_eq!(event.payload, Vec::<u8>::new());
        send(peer, "info", json!({"satellite":{"name":"synthetic"}})).await;
    }
}

#[tokio::test]
#[ignore = "T3 loopback Wyoming probe during playback"]
async fn t3_m4_playback_answers_queued_probe_without_losing_audio_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut sender = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (mut peer, _) = listener.accept().await.unwrap();
    let task = tokio::spawn(async move {
        send(&mut sender, "describe", Value::Null).await;
        let info = receive(&mut sender, &mut Decoder::new()).await;
        assert_eq!(info.header.r#type, "info");
        assert_eq!(info.header.data, Some(json!({"satellite":{"name":"synthetic"}})));
        send(&mut sender, "audio-start", json!({"rate":24000,"width":2,"channels":1})).await;
        send(&mut sender, "audio-stop", Value::Null).await;
    });
    let mut decoder = Decoder::new();
    assert_eq!(
        receive_playback(&mut peer, &mut decoder).await.header.r#type,
        "audio-start"
    );
    assert_eq!(
        receive_playback(&mut peer, &mut decoder).await.header.r#type,
        "audio-stop"
    );
    task.await.unwrap();
}
