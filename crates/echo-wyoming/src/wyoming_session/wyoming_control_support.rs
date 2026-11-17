use super::wyoming_peer::{receive, send};
use echo_contracts::wyoming::{AudioFormat, WyomingEvent};
use echo_wyoming::{Error, EventStream, PlayOutcome, WyomingClient};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    sync::mpsc,
};

fn format() -> AudioFormat {
    serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap()
}

async fn pending_until_ping(
    play: &mut std::pin::Pin<Box<impl Future<Output = Result<PlayOutcome, Error>>>>,
    events: &mut EventStream,
) -> Option<echo_wyoming::Run> {
    let mut run = None;
    loop {
        let mut next = Box::pin(events.recv());
        let event = std::future::poll_fn(|cx| {
            assert!(play.as_mut().poll(cx).is_pending(), "completed without played");
            next.as_mut().poll(cx)
        })
        .await
        .unwrap()
        .unwrap();
        if event.run.is_some() {
            run = event.run;
        }
        if matches!(event.event, WyomingEvent::Ping(_)) {
            return run;
        }
    }
}

pub async fn cut_duplex<T: AsyncRead + AsyncWrite + Unpin>(mut client: WyomingClient, mut peer: T) {
    let mut events = client.events().unwrap();
    let (tx, rx) = mpsc::channel(8);
    tx.send(vec![1, 0]).await.unwrap();
    let caller = async {
        let mut play = Box::pin(client.play(format(), rx));
        let mut run = pending_until_ping(&mut play, &mut events).await.unwrap();
        assert_eq!(
            run.stats(),
            echo_wyoming::RunStats {
                received: 5,
                dropped: 3
            }
        );
        assert_eq!(run.recv().await.unwrap().unwrap().payload, [4]);
        assert_eq!(run.recv().await.unwrap().unwrap().payload, [5]);
        for value in 10..15 {
            tx.try_send(vec![value]).unwrap();
        }
        client.cut().await.unwrap();
        client.cut().await.unwrap();
        assert_eq!(
            client.describe().await.unwrap(),
            WyomingEvent::Info(json!({"cut":true}))
        );
        assert_eq!(play.await, Ok(PlayOutcome::Cut));
        assert_eq!(run.recv().await.unwrap().unwrap().payload, [6]);
        assert_eq!(run.recv().await.unwrap().unwrap().payload, [7]);
        assert_eq!(run.recv().await.unwrap(), None);
        assert_eq!(run.stats().received, 7);
        client.cut().await.unwrap();
        client.close().await.unwrap();
    };
    tokio::join!(caller, cut_server(&mut peer));
}

async fn cut_server<T: AsyncRead + AsyncWrite + Unpin>(mut peer: T) {
    reply_prefix(&mut peer).await;
    send(
        &mut peer,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
        vec![],
    )
    .await;
    mic_five(&mut peer).await;
    assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-continue");
    assert_eq!(receive(&mut peer).await.header.r#type, "audio-stop");
    assert_eq!(receive(&mut peer).await.header.r#type, "describe");
    send(&mut peer, "info", json!({"cut":true}), vec![]).await;
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![6],
    )
    .await;
    continued_tail(&mut peer).await;
}

async fn continued_tail<T: AsyncRead + AsyncWrite + Unpin>(mut peer: T) {
    send(&mut peer, "played", Value::Null, vec![]).await;
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![7],
    )
    .await;
    send(&mut peer, "audio-stop", Value::Null, vec![]).await;
    assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
}

async fn mic_five<T: AsyncWrite + Unpin>(peer: &mut T) {
    for value in 1..=5 {
        send(
            peer,
            "audio-chunk",
            json!({"rate":16000,"width":2,"channels":1}),
            vec![value],
        )
        .await;
    }
    send(peer, "ping", Value::Null, vec![]).await;
}

pub async fn pause_resume<T: AsyncRead + AsyncWrite + Unpin>(mut client: WyomingClient, mut peer: T) {
    let mut events = client.events().unwrap();
    let caller = async {
        client.cut().await.unwrap();
        client.run_satellite().await.unwrap();
        let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
        client.pause().await.unwrap();
        client.pause().await.unwrap();
        assert_eq!(run.recv().await.unwrap(), None);
        assert_eq!(run.stats().received, 0);
        assert_eq!(
            client.describe().await.unwrap(),
            WyomingEvent::Info(json!({"paused":true}))
        );
        assert!(matches!(
            events.recv().await.unwrap().unwrap().event,
            WyomingEvent::Info(_)
        ));
        client.run_satellite().await.unwrap();
        let mut next = events.recv().await.unwrap().unwrap().run.unwrap();
        assert_eq!(next.stats().received, 0);
        client.close().await.unwrap();
        assert_eq!(next.recv().await.unwrap(), None);
    };
    let server = async {
        let pipeline = json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"});
        assert_eq!(receive(&mut peer).await.header.r#type, "run-satellite");
        send(&mut peer, "run-pipeline", pipeline.clone(), vec![]).await;
        assert_eq!(receive(&mut peer).await.header.r#type, "pause-satellite");
        assert_eq!(receive(&mut peer).await.header.r#type, "describe");
        send(&mut peer, "run-pipeline", pipeline.clone(), vec![]).await;
        send(
            &mut peer,
            "audio-chunk",
            json!({"rate":16000,"width":2,"channels":1}),
            vec![1],
        )
        .await;
        send(&mut peer, "info", json!({"paused":true}), vec![]).await;
        assert_eq!(receive(&mut peer).await.header.r#type, "run-satellite");
        send(&mut peer, "run-pipeline", pipeline, vec![]).await;
        assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}

async fn reply_prefix<T: AsyncRead + Unpin>(peer: &mut T) {
    assert_eq!(receive(peer).await.header.r#type, "audio-start");
    assert_eq!(receive(peer).await.payload, [1, 0]);
}
