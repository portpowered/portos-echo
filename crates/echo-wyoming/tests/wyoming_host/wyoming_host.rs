//! Private T3 prebuilt-process controller and public client witness.
use echo_contracts::wyoming::{AudioFormat, WyomingEvent};
use echo_wyoming::{ClientOptions, EventStream, PlayOutcome, Run, WyomingClient};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};
use tokio::sync::mpsc;

struct Host {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Host {
    fn size(&mut self) -> usize {
        writeln!(self.input.as_mut().unwrap(), "size").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        self.line().parse().unwrap()
    }
    fn start() -> (Self, u16) {
        let mut child = controller()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut host = Self { child, input, output };
        let port = host.line().parse().unwrap();
        (host, port)
    }
    fn line(&mut self) -> String {
        let mut line = String::new();
        assert_ne!(self.output.read_line(&mut line).unwrap(), 0, "controller exited early");
        line.trim().to_owned()
    }
    fn command(&mut self, command: &str) {
        writeln!(self.input.as_mut().unwrap(), "{command}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        assert_eq!(self.line(), command);
    }
    fn finish(mut self) {
        drop(self.input.take());
        assert_eq!(self.line(), "cleaned");
        assert_eq!(self.child.wait().unwrap().code(), Some(0));
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

async fn wake(host: &mut Host, events: &mut EventStream) -> Run {
    host.command("wake");
    loop {
        let event = events.recv().await.unwrap().unwrap();
        if let Some(mut run) = event.run {
            assert!(matches!(event.event, WyomingEvent::RunPipeline(_)));
            for _ in 0..5 {
                let chunk = run.recv().await.unwrap().unwrap();
                assert_eq!((chunk.data.rate, chunk.data.width, chunk.data.channels), (16000, 2, 1));
                assert_eq!(chunk.payload.as_ref(), &[0; 2048]);
            }
            writeln!(
                std::io::stdout(),
                "host wake: mic_chunks={} selected=5 chunk_bytes=2048 format=16000/2/1",
                run.stats().received
            )
            .unwrap();
            return run;
        }
        assert!(!matches!(event.event, WyomingEvent::AudioStart(_)));
    }
}

async fn reply(client: &WyomingClient, events: &mut EventStream) {
    let format: AudioFormat = serde_json::from_value(serde_json::json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let (tx, rx) = mpsc::channel(10);
    for _ in 0..10 {
        tx.send([1, 0].repeat(2400)).await.unwrap();
    }
    drop(tx);
    let play = client.play(format, rx);
    let drain = async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if matches!(event.event, WyomingEvent::Played(_)) {
                break;
            }
        }
    };
    let (outcome, ()) = tokio::join!(play, drain);
    assert_eq!(outcome, Ok(PlayOutcome::Played));
    writeln!(
        std::io::stdout(),
        "host reply: reply_chunks=10 chunk_bytes=4800 played=1"
    )
    .unwrap();
}

pub(super) async fn lifecycle() {
    let (mut host, port) = Host::start();
    let mut client = WyomingClient::connect(([127, 0, 0, 1], port).into(), ClientOptions::default())
        .await
        .unwrap();
    let mut events = client.events().unwrap();
    let info = client.describe().await.unwrap();
    let info = match info {
        WyomingEvent::Info(info) => info,
        _ => serde_json::Value::Null,
    };
    assert_eq!(info["satellite"]["name"], "Wyoming Test");
    writeln!(std::io::stdout(), "host describe: satellite=Wyoming Test").unwrap();
    client.run_satellite().await.unwrap();
    let mut first = wake(&mut host, &mut events).await;
    reply(&client, &mut events).await;
    end_capture(&client, &mut first).await;
    host.command("stop");
    host.command("restart");
    let second = wake(&mut host, &mut events).await;
    assert_eq!(first.recv().await.unwrap(), None);
    assert!(second.stats().received >= 5);
    writeln!(std::io::stdout(), "host restart: fresh_run=true old_run_closed=true").unwrap();
    client.close().await.unwrap();
    host.finish();
}

async fn end_capture(client: &WyomingClient, run: &mut Run) {
    client.end_run().await.unwrap();
    while run.recv().await.unwrap().is_some() {}
}

pub(super) async fn controls() {
    let (mut host, port) = Host::start();
    let mut client = WyomingClient::connect(([127, 0, 0, 1], port).into(), ClientOptions::default())
        .await
        .unwrap();
    let mut events = client.events().unwrap();
    client.run_satellite().await.unwrap();
    let mut first = wake(&mut host, &mut events).await;
    client.pause().await.unwrap();
    while first.recv().await.unwrap().is_some() {}
    client.run_satellite().await.unwrap();
    let second = wake(&mut host, &mut events).await;
    let format = serde_json::from_value(serde_json::json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let (tx, rx) = mpsc::channel(10);
    for _ in 0..10 {
        tx.send([1, 0].repeat(2400)).await.unwrap();
    }
    let play = client.play(format, rx);
    let control = async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            assert!(!matches!(
                event.event,
                WyomingEvent::Played(_) | WyomingEvent::RunPipeline(_)
            ));
            if matches!(event.event, WyomingEvent::AudioChunk(_)) && host.size() >= 4800 {
                break;
            }
        }
        let before = second.stats().received;
        client.cut().await.unwrap();
        let mut mic_after_cut = 0;
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if matches!(event.event, WyomingEvent::AudioChunk(_)) {
                mic_after_cut += 1;
            }
            if matches!(event.event, WyomingEvent::Played(_)) {
                break;
            }
            assert!(!matches!(event.event, WyomingEvent::RunPipeline(_)));
        }
        mic_after_cut += capture_after_played(&mut events).await;
        assert!(mic_after_cut >= 1);
        assert!(second.stats().received > before);
        writeln!(
            std::io::stdout(),
            "host control: pause_resume=true cut=flush played=1 duplex_chunks_after_cut={mic_after_cut}"
        )
        .unwrap();
    };
    let (result, ()) = tokio::join!(play, control);
    assert_eq!(result, Ok(PlayOutcome::Cut));
    drop(tx);
    client.close().await.unwrap();
    host.finish();
}

// Flush can finish before the next capture slot: require capture after Played.
async fn capture_after_played(events: &mut EventStream) -> usize {
    loop {
        let event = events.recv().await.unwrap().unwrap();
        assert!(!matches!(event.event, WyomingEvent::RunPipeline(_)));
        if matches!(event.event, WyomingEvent::AudioChunk(_)) {
            return 1;
        }
    }
}

fn controller() -> Command {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let shell = std::path::PathBuf::from("sh");
    let mut command = Command::new(&shell);

    command.arg("tests/wyoming_host.sh").current_dir(root);
    command
}

pub(super) fn failed_startup_cleanup() {
    let output = controller().arg("--startup-failure").output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "startup-failed-cleaned"
    );
}
