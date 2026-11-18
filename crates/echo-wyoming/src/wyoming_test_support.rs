use echo_contracts::wyoming::{AudioFormat, Decoder, Event, Header, WyomingEvent, encode};
use echo_wyoming::{PlayOutcome, WyomingClient};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};

pub fn frame(kind: &str, data: Value, payload: Vec<u8>) -> Event {
    Event {
        header: Header {
            r#type: kind.into(),
            data: Some(data),
            data_length: 0,
            payload_length: 0,
        },
        data: None,
        payload,
    }
}

pub async fn receive<T: AsyncRead + Unpin>(peer: &mut T) -> Event {
    let mut decoder = Decoder::new();
    loop {
        let byte = peer.read_u8().await.unwrap();
        if let Some(event) = decoder.feed(&[byte]).unwrap().pop() {
            return event;
        }
    }
}

pub async fn send<T: AsyncWrite + Unpin>(peer: &mut T, kind: &str, data: Value, payload: Vec<u8>) {
    peer.write_all(&encode(&frame(kind, data, payload))).await.unwrap();
}

pub async fn microphone<T: AsyncRead + AsyncWrite + Unpin>(mut client: WyomingClient, mut peer: T) {
    let mut events = client.events().unwrap();
    tokio::join!(microphone_caller(&client, &mut events), microphone_server(&mut peer));
}

async fn microphone_caller(client: &WyomingClient, events: &mut echo_wyoming::EventStream) {
    describe(client, events, json!({"satellite":{"name":"wyoming-fixture"}})).await;
    client.run_satellite().await.unwrap();
    let event = next(events).await;
    assert!(matches!(event.event, WyomingEvent::RunPipeline(_)));
    let mut first = event.run.unwrap();
    assert_eq!(first.pipeline().wake_word_name, "alexa");
    for _ in 0..5 {
        let frame = first.recv().await.unwrap().unwrap();
        assert_eq!((frame.data.rate, frame.data.width, frame.data.channels), (16000, 2, 1));
        assert_eq!(frame.payload, vec![0; 2048]);
    }
    microphone_replacement(client, events, first).await;
}

async fn microphone_replacement(
    client: &WyomingClient,
    events: &mut echo_wyoming::EventStream,
    mut first: echo_wyoming::Run,
) {
    let unknown = next_control(events).await;
    assert_eq!(
        unknown.event,
        WyomingEvent::Other("future-event".into(), json!({"x":1}))
    );
    assert_eq!(unknown.payload, [7, 8]);
    let mut second = next(events).await.run.unwrap();
    assert_eq!(first.recv().await.unwrap(), None);
    assert_eq!(first.stats().received, 5);
    assert_eq!(second.stats().received, 0);
    describe(client, events, json!({"barrier":true})).await;
    let _ = events.recv().await.unwrap();
    assert_eq!(second.recv().await.unwrap().unwrap().payload, [9, 10]);
    assert_eq!(second.stats().received, 1);
    client.close().await.unwrap();
}

async fn microphone_server<T: AsyncRead + AsyncWrite + Unpin>(peer: &mut T) {
    assert_kind(peer, "describe").await;
    send(peer, "info", json!({"satellite":{"name":"wyoming-fixture"}}), vec![]).await;
    assert_kind(peer, "run-satellite").await;
    microphone_frames(peer).await;
}

async fn microphone_frames<T: AsyncRead + AsyncWrite + Unpin>(peer: &mut T) {
    let fixture = include_str!("../../../tests/fixtures/wyoming/wake-word.jsonl");
    let row: Value = serde_json::from_str(fixture.lines().nth(1).unwrap()).unwrap();
    let pipeline = row["header"]["data"].clone();
    let bytes = encode(&frame("run-pipeline", pipeline.clone(), vec![]));
    for part in bytes.chunks(3) {
        peer.write_all(part).await.unwrap();
    }
    for _ in 0..5 {
        send(
            peer,
            "audio-chunk",
            json!({"rate":16000,"width":2,"channels":1}),
            vec![0; 2048],
        )
        .await;
    }
    let mut coalesced = encode(&frame("future-event", json!({"x":1}), vec![7, 8]));
    coalesced.extend(encode(&frame("run-pipeline", pipeline, vec![])));
    peer.write_all(&coalesced).await.unwrap();
    assert_kind(peer, "describe").await;
    send(peer, "info", json!({"barrier":true}), vec![]).await;
    send(
        peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![9, 10],
    )
    .await;
    assert_kind(peer, "hassmic-end").await;
    assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
}

pub async fn playback<T: AsyncRead + AsyncWrite + Unpin>(mut client: WyomingClient, mut peer: T) {
    let mut events = client.events().unwrap();
    tokio::join!(playback_caller(&client, &mut events), playback_server(&mut peer));
}

async fn playback_caller(client: &WyomingClient, events: &mut echo_wyoming::EventStream) {
    for count in [0, 1, 10] {
        let format = AudioFormat {
            rate: 24000,
            width: 2,
            channels: 1,
            extra: std::collections::BTreeMap::default(),
        };
        let (tx, rx) = mpsc::channel(16);
        for _ in 0..count {
            tx.send([1, 0].repeat(2400)).await.unwrap();
        }
        drop(tx);
        let mut play = Box::pin(client.play(format, rx));
        loop {
            let mut next = Box::pin(events.recv());
            let event = std::future::poll_fn(|cx| {
                assert!(play.as_mut().poll(cx).is_pending(), "play completed before played");
                next.as_mut().poll(cx)
            })
            .await
            .unwrap()
            .unwrap();
            if matches!(event.event, WyomingEvent::Ping(_)) {
                break;
            }
        }
        // Describe tells the peer that the pending assertion has completed.
        let (described, result) = tokio::join!(client.describe(), play);
        assert_eq!(described.unwrap(), WyomingEvent::Info(json!({"ack":count})));
        assert_eq!(result.unwrap(), PlayOutcome::Played);
        loop {
            if matches!(next(events).await.event, WyomingEvent::Info(_)) {
                break;
            }
        }
    }
    client.close().await.unwrap();
}

async fn playback_server<T: AsyncRead + AsyncWrite + Unpin>(peer: &mut T) {
    for count in [0, 1, 10] {
        assert_eq!(
            typed(peer).await,
            WyomingEvent::AudioStart(AudioFormat {
                rate: 24000,
                width: 2,
                channels: 1,
                extra: std::collections::BTreeMap::default()
            })
        );
        for _ in 0..count {
            let chunk = receive(peer).await;
            assert_eq!(chunk.header.r#type, "audio-chunk");
            assert_pcm(&chunk.payload);
        }
        assert_kind(peer, "audio-stop").await;
        send(peer, "ping", Value::Null, vec![]).await;
        assert_kind(peer, "describe").await;
        send(peer, "played", Value::Null, vec![]).await;
        send(peer, "info", json!({"ack":count}), vec![]).await;
    }
    assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
}

async fn next(events: &mut echo_wyoming::EventStream) -> echo_wyoming::ReceivedEvent {
    events.recv().await.unwrap().unwrap()
}

async fn typed<T: AsyncRead + Unpin>(peer: &mut T) -> WyomingEvent {
    receive(peer).await.typed().unwrap()
}

async fn describe(client: &WyomingClient, events: &mut echo_wyoming::EventStream, data: Value) {
    assert_eq!(client.describe().await.unwrap(), WyomingEvent::Info(data));
    assert!(matches!(next(events).await.event, WyomingEvent::Info(_)));
}

async fn assert_kind<T: AsyncRead + Unpin>(peer: &mut T, kind: &str) {
    assert_eq!(receive(peer).await.header.r#type, kind);
}

// Cardinality of audio notifications is intentionally coalesced. Exact frame
// values and counts above remain observed through the canonical Run handle.
async fn next_control(events: &mut echo_wyoming::EventStream) -> echo_wyoming::ReceivedEvent {
    loop {
        let event = next(events).await;
        if !matches!(event.event, WyomingEvent::AudioChunk(_)) {
            return event;
        }
        assert_eq!(event.payload, Vec::<u8>::new());
    }
}

fn assert_pcm(payload: &[u8]) {
    assert_eq!(payload, [1, 0].repeat(2400));
}
