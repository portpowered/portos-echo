//! M7-only audible dialogue; production Live protocol and shared fake stay fixed.
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use echo_live::{
    LivePeer,
    fake::{DialogueCall, DialogueSent},
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

type Offer = (String, oneshot::Sender<Result<String, String>>);

pub struct Live {
    base: String,
    observed: watch::Receiver<Vec<DialogueCall>>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), String>>,
}

impl Live {
    pub async fn bind(text: &'static str) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|_| "m7: Live bind")?;
        let base = format!("http://{}", listener.local_addr().map_err(|_| "m7: Live address")?);
        let (send, receive) = mpsc::channel(1);
        let (observed, observations) = watch::channel(vec![]);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let app = Router::new().route("/v1/live/sessions", post(offer)).with_state(send);
            let work = async {
                tokio::try_join!(
                    async {
                        axum::serve(listener, app)
                            .await
                            .map_err(|_| "m7: signaling server".to_owned())
                    },
                    run(receive, observed, text)
                )
                .map(|_| ())
            };
            tokio::select! { result = work => result, _ = stopped => Ok(()) }
        });
        Ok(Self {
            base,
            observed: observations,
            stop: Some(stop),
            task,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn calls(&self) -> Vec<DialogueCall> {
        self.observed.borrow().clone()
    }

    pub async fn completed(&mut self) -> Result<(), String> {
        loop {
            if self.observed.borrow().first().is_some_and(|call| call.reply_completed) {
                return Ok(());
            }
            self.observed
                .changed()
                .await
                .map_err(|_| "m7: Live observations closed")?;
        }
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.map_err(|_| "m7: Live owner failed")?
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn offer(State(send): State<mpsc::Sender<Offer>>, Json(body): Json<Value>) -> Result<String, StatusCode> {
    let sdp = body["sdp"].as_str().ok_or(StatusCode::BAD_REQUEST)?.to_owned();
    let (reply, answer) = oneshot::channel();
    send.send((sdp, reply))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    answer
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|_| StatusCode::BAD_REQUEST)
}

async fn run(
    mut offers: mpsc::Receiver<Offer>,
    observed: watch::Sender<Vec<DialogueCall>>,
    text: &str,
) -> Result<(), String> {
    let (sdp, answer) = offers.recv().await.ok_or("m7: missing offer")?;
    drop(offers);
    observed.send_replace(vec![DialogueCall {
        call_index: 0,
        delegation_id: "m7-d-1".into(),
        sent: vec![],
        rejected_commentary: 0,
        received_audio: 0,
        reply_completed: false,
        closed: false,
        failure: None,
    }]);
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .map_err(|_| "m7: media bind")?;
    let negotiated = peer.answer(&sdp).await.map_err(|_| "m7: SDP answer".to_owned());
    let admitted = negotiated.is_ok();
    let _ = answer.send(negotiated);
    let result = if admitted {
        dialogue(&mut peer, &observed, text).await
    } else {
        Err("m7: negotiation failed".into())
    };
    peer.close();
    observed.send_modify(|calls| {
        calls[0].closed = true;
        calls[0].failure = result.clone().err();
    });
    result
}

async fn dialogue(peer: &mut LivePeer, observed: &watch::Sender<Vec<DialogueCall>>, text: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut delegated = false;
    let mut replied = false;
    loop {
        if Instant::now() >= deadline {
            return Err("m7: media deadline".into());
        }
        drive(peer, deadline).await?;
        let mut received = 0;
        while peer.pop_audio().is_some() {
            received += 1;
        }
        observed.send_modify(|calls| calls[0].received_audio += received);
        if peer.ready() && !delegated && observed.borrow()[0].received_audio >= 44 {
            send(
                peer,
                observed,
                DialogueSent::Text(
                    json!({"type":"session.delegation.created",
                "delegation":{"id":"m7-d-1","target":"client","input":"Ask Muse to run the test job"}})
                    .to_string(),
                ),
            )
            .await?;
            delegated = true;
        }
        while let Some(message) = peer.pop_text() {
            let value: Value = serde_json::from_str(&message).map_err(|_| "m7: invalid commentary")?;
            if delegated
                && !replied
                && value["type"] == "session.commentary.append"
                && value["delegation_id"] == "m7-d-1"
                && value["text"] == text
            {
                replied = true;
                reply(peer, observed, text).await?;
            } else {
                observed.send_modify(|calls| calls[0].rejected_commentary += 1);
                return Err("m7: unmatched commentary".into());
            }
        }
    }
}

async fn reply(peer: &mut LivePeer, observed: &watch::Sender<Vec<DialogueCall>>, text: &str) -> Result<(), String> {
    send(
        peer,
        observed,
        DialogueSent::Text(json!({"type":"response.audio_transcript.done","transcript":text}).to_string()),
    )
    .await?;
    // The legacy Muse media path emits a frame for every packet; unlike the
    // Codex path, it does not discard a resampler warm-up frame.
    for sequence in 0..10 {
        let frame = crate::lab_run::sim::m2::codex::tone::packet(sequence).ok_or("m7: tone sequence")?;
        send(peer, observed, DialogueSent::Audio(frame)).await?;
        let next = Instant::now() + Duration::from_millis(20);
        while Instant::now() < next {
            drive(peer, next).await?;
        }
    }
    send(
        peer,
        observed,
        DialogueSent::Text(json!({"type":"response.audio.done"}).to_string()),
    )
    .await?;
    observed.send_modify(|calls| calls[0].reply_completed = true);
    Ok(())
}

async fn send(
    peer: &mut LivePeer,
    observed: &watch::Sender<Vec<DialogueCall>>,
    output: DialogueSent,
) -> Result<(), String> {
    match &output {
        DialogueSent::Text(text) => peer.send_text(text).await.map_err(|_| "m7: send text")?,
        DialogueSent::Audio(frame) => peer
            .send_audio(frame.clone(), Instant::now().into_std())
            .await
            .map_err(|_| "m7: send tone")?,
    }
    observed.send_modify(|calls| calls[0].sent.push(output));
    Ok(())
}

async fn drive(peer: &mut LivePeer, ceiling: Instant) -> Result<(), String> {
    let next = Instant::from_std(peer.flush().await.map_err(|_| "m7: media flush")?).min(ceiling);
    tokio::select! {
        ready = peer.readable() => {
            ready.map_err(|_| "m7: media readiness")?;
            peer.receive(Instant::now().into_std()).await.map_err(|_| "m7: receive media")?;
        },
        () = tokio::time::sleep_until(next) => peer.timeout(Instant::now().into_std()).await.map_err(|_| "m7: media timeout")?,
    }
    Ok(())
}
