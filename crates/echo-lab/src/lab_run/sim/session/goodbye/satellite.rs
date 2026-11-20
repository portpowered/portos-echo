//! Controlled duplex Wyoming peer using the existing M4 wire observer.
use crate::lab_run::sim::m4::wire::{Timing, frame};
use echo_contracts::wyoming::{Event, Header, encode};
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

type Input = (bool, oneshot::Sender<Result<(), String>>);

pub(super) struct Satellite {
    pub address: SocketAddr,
    pub observed: watch::Receiver<Timing>,
    inputs: mpsc::Sender<Input>,
    task: JoinHandle<Result<(), String>>,
}

impl Satellite {
    pub async fn bind() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "goodbye: Wyoming bind failed")?;
        let address = listener.local_addr().map_err(|_| "goodbye: missing Wyoming address")?;
        let (observe, observed) = watch::channel(Timing::default());
        let (inputs, receive) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            let (peer, _) = listener.accept().await.map_err(|_| "goodbye: Wyoming accept failed")?;
            serve(peer, receive, observe).await
        });
        Ok(Self {
            address,
            observed,
            inputs,
            task,
        })
    }

    pub async fn speak(&self, wake: bool) -> Result<(), String> {
        let (sent, received) = oneshot::channel();
        self.inputs
            .send((wake, sent))
            .await
            .map_err(|_| "goodbye: Wyoming peer ended")?;
        received.await.map_err(|_| "goodbye: missing capture receipt")?
    }

    pub async fn close(&mut self) -> Result<(), String> {
        self.task.abort();
        match (&mut self.task).await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(_) => Err("goodbye: Wyoming peer failed".into()),
        }
    }
}

impl Drop for Satellite {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    peer: TcpStream,
    mut inputs: mpsc::Receiver<Input>,
    observed: watch::Sender<Timing>,
) -> Result<(), String> {
    let (mut read, mut write) = peer.into_split();
    let mut timing = Timing::default();
    let mut playing = false;
    let started = Instant::now();
    loop {
        let next = frame(&mut read);
        tokio::pin!(next);
        let event = loop {
            tokio::select! {
                result = &mut next => break result?,
                input = inputs.recv() => {
                    let Some((wake, sent)) = input else { return Ok(()); };
                    let result = capture(&mut write, &mut timing, started, wake).await;
                    observed.send_replace(timing.clone());
                    let _ = sent.send(result.clone());
                    result?;
                }
            }
        };
        let Some((_, event)) = event else {
            return Ok(());
        };
        timing.playback(&event, started.elapsed())?;
        match event.header.r#type.as_str() {
            "describe" => {
                send(&mut write, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await?;
            }
            "audio-start" => playing = true,
            "audio-stop" | "hassmic-flush" if playing => {
                let played = send(&mut write, "played", json!({}), vec![]).await?;
                timing.capture(&played, usize::MAX, started.elapsed())?;
                playing = false;
            }
            "hassmic-end" => {
                send(&mut write, "audio-stop", json!({}), vec![]).await?;
            }
            _ => {}
        }
        observed.send_replace(timing.clone());
    }
}

async fn capture(
    write: &mut (impl AsyncWrite + Unpin),
    timing: &mut Timing,
    started: Instant,
    wake: bool,
) -> Result<(), String> {
    let format = json!({"rate":16000,"width":2,"channels":1});
    if wake {
        let event = send(
            write,
            "run-pipeline",
            json!({
                "start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"
            }),
            vec![],
        )
        .await?;
        timing.capture(&event, usize::MAX, started.elapsed())?;
        send(write, "audio-start", format.clone(), vec![]).await?;
    }
    for bytes in crate::lab_run::sim::m3::fixture().chunks_exact(640) {
        let event = send(write, "audio-chunk", format.clone(), bytes.to_vec()).await?;
        timing.capture(&event, usize::MAX, started.elapsed())?;
    }
    Ok(())
}

async fn send(
    write: &mut (impl AsyncWrite + Unpin),
    kind: &str,
    data: Value,
    payload: Vec<u8>,
) -> Result<Event, String> {
    let event = Event {
        header: Header {
            r#type: kind.into(),
            data: Some(data),
            data_length: 0,
            payload_length: payload.len(),
        },
        data: None,
        payload,
    };
    write
        .write_all(&encode(&event))
        .await
        .map_err(|_| "goodbye: Wyoming write failed")?;
    Ok(event)
}
