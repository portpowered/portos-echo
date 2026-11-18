//! Keep capture and the run owned while a settled reply waits for activity.
use super::{Error, Event, MediaEvent, Notice, Options, Source, VoicePort};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use std::future::Future;

pub(super) async fn replies<L: EchoLink>(
    client: &L,
    input: (&mut L::Events, &mut <L::Events as LinkEvents>::Run),
    port: &mut impl VoicePort,
    context: (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    options: &Options,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let (events, microphone) = input;
    let (source, drain) = context;
    let mut generation_options = *options;
    let mut previous = super::Stats::default();
    tokio::pin!(cancelled);
    loop {
        let result = super::generation(
            client,
            (events, microphone),
            port,
            (source, drain),
            &generation_options,
            cancelled.as_mut(),
            observe,
        )
        .await;
        if result == Err(Error::Control) && source.controller.cutting() {
            source.controller.cut_settled();
            generation_options.response_deadline = source.controller.replacement_deadline()?;
            observe(Notice::ReplyPaused)?;
            continue;
        }
        result?;
        let Some(timeout) = options.listen_timeout else {
            return Ok(());
        };
        source.controller.listen_after_reply(timeout)?;
        let mut stats = source.stats.clone();
        stats.reply_dropped += source.reply.dropped_frames();
        let totals = stats.clone();
        stats.reply_received -= previous.reply_received;
        stats.reply_sent -= previous.reply_sent;
        stats.reply_dropped -= previous.reply_dropped;
        previous = totals;
        observe(Notice::ReplySettled(stats))?;
        observe(Notice::ReplyPlayed)?;
        if !listen((events, microphone), port, source, cancelled.as_mut(), observe).await? {
            return Ok(());
        }
        generation_options.response_deadline = source.controller.replacement_deadline()?;
    }
}

async fn listen<E: LinkEvents>(
    input: (&mut E, &mut E::Run),
    port: &mut impl VoicePort,
    source: &mut Source,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<bool, Error> {
    let (events, microphone) = input;
    let deadline = source.controller.idle_deadline().ok_or(Error::Input)?;
    tokio::pin!(cancelled);
    loop {
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            () = tokio::time::sleep_until(deadline) => return Ok(false),
            result = listening_step(events, microphone, port, source, observe) => {
                if result? { return Ok(true); }
            }
        }
        tokio::task::yield_now().await;
    }
}

async fn listening_step<E: LinkEvents>(
    events: &mut E,
    microphone: &mut E::Run,
    port: &mut impl VoicePort,
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<bool, Error> {
    tokio::select! {
        event = events.recv() => super::device(&event?.ok_or(Error::Wyoming)?, true, source, observe)?,
        frame = super::earcon::microphone(microphone) => {
            let frame = frame?.ok_or(Error::Wyoming)?;
            if source.capture(Some(frame), port)? {
                observe(Notice::Device(LinkEvent::MicrophoneDropped(1)))?;
            }
        },
        event = port.next() => {
            let event = event?;
            if activity(&event) {
                source.resume_reply()?;
                super::media(event, port, source, observe).await?;
                return Ok(true);
            }
            match event {
                MediaEvent::Control(Event::Closed | Event::Tool { .. }) => return Err(Error::Unavailable),
                // Continuous RTP and stale done frames cannot renew listening.
                MediaEvent::Control(_) | MediaEvent::Audio(_) => (),
            }
        },
    }
    Ok(false)
}

fn activity(event: &MediaEvent) -> bool {
    matches!(
        event,
        MediaEvent::Control(Event::SpeechStarted | Event::TurnStarted { .. })
    ) || matches!(event, MediaEvent::Control(Event::Transcript { role, done, .. })
            if role == "user" || (role == "assistant" && !done))
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_link::{
        LinkConnector,
        fake::{Command, FakeConnector, Observer},
    };
    use std::{
        cell::{Cell, RefCell},
        future::poll_fn,
        pin::Pin,
        task::Poll,
        time::Duration,
    };
    use tokio::{sync::mpsc, time::Instant};

    struct Port(mpsc::Receiver<MediaEvent>);
    impl VoicePort for Port {
        fn send(&self, _: echo_media::PcmFrame) -> Result<(), Error> {
            Ok(())
        }
        async fn next(&mut self) -> Result<MediaEvent, Error> {
            self.0.recv().await.ok_or(Error::Unavailable)
        }
        async fn ready_audio(&mut self) -> Result<Option<echo_media::PcmFrame>, Error> {
            Ok(None)
        }
    }

    async fn poll(work: Pin<&mut impl Future<Output = Result<super::super::Stats, Error>>>) {
        let mut work = work;
        for _ in 0..128 {
            poll_fn(|cx| {
                assert!(work.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
    }

    async fn reply(tx: &mpsc::Sender<MediaEvent>, level: i16) {
        tx.send(MediaEvent::Control(Event::Transcript {
            role: "assistant".into(),
            text: "fixture".into(),
            done: false,
        }))
        .await
        .unwrap();
        for _ in 0..12 {
            tx.send(MediaEvent::Audio(Box::new([level; 320]))).await.unwrap();
        }
        tx.send(MediaEvent::Control(Event::ReplyDone { text: "fixture".into() }))
            .await
            .unwrap();
    }

    // T1: the real run loop and PCM converters, memory-only device/media,
    // explicit speaker receipts and paused time. No native-host claim.
    #[tokio::test(start_paused = true)]
    async fn voice_listen_two_replies_keep_one_run_then_expire_without_noise_renewal() {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        observer.wake("Computer", vec![]);
        let Some(LinkEvent::Wake(microphone)) = events.recv().await.unwrap() else {
            unreachable!()
        };
        let (tx, rx) = mpsc::channel(64);
        let mut port = Port(rx);
        let receipts = Cell::new(0);
        let reply_stats = RefCell::new(Vec::new());
        let settled = Cell::new(Instant::now());
        let mut work = Box::pin(super::super::run(
            &client,
            &mut events,
            microphone,
            &mut port,
            Options {
                listen_timeout: Some(Duration::from_secs(3)),
                reply_lead_ms: 200,
                response_deadline: Instant::now() + Duration::from_secs(10),
                played_timeout: Duration::from_secs(1),
                muted: false,
            },
            std::future::pending(),
            |notice| {
                if let Notice::ReplySettled(stats) = &notice {
                    reply_stats.borrow_mut().push((stats.reply_received, stats.reply_sent));
                }
                if matches!(notice, Notice::ReplyPlayed) {
                    receipts.set(receipts.get() + 1);
                    settled.set(Instant::now());
                }
                Ok(())
            },
        ));
        for number in 1..=2 {
            reply(&tx, if number == 1 { 5000 } else { -5000 }).await;
            settle(&mut work, &observer, &receipts, number).await;
            assert!(!observer.commands().contains(&Command::EndRun));
            assert_eq!(
                observer
                    .commands()
                    .iter()
                    .filter(|c| matches!(c, Command::Stop))
                    .count(),
                number
            );
            if number == 1 {
                tokio::time::advance(Duration::from_secs(2)).await;
                tx.send(MediaEvent::Control(Event::SpeechStarted)).await.unwrap();
                poll(work.as_mut()).await;
            }
        }
        idle_noise(&tx).await;
        poll(work.as_mut()).await;
        tokio::time::advance(Duration::from_secs(3) - Duration::from_nanos(1)).await;
        poll(work.as_mut()).await;
        assert!(!observer.commands().contains(&Command::EndRun));
        tokio::time::advance(Duration::from_nanos(1)).await;
        let stats = work.await.unwrap();
        assert_eq!(Instant::now() - settled.get(), Duration::from_secs(3));
        assert_eq!(receipts.get(), 2);
        assert_eq!(*reply_stats.borrow(), [(12, 12), (12, 12)]);
        assert_eq!(stats.reply_sent, 24);
        assert_eq!(
            observer
                .commands()
                .iter()
                .filter(|c| matches!(c, Command::EndRun))
                .count(),
            1
        );
        let frames: Vec<_> = observer
            .commands()
            .into_iter()
            .filter_map(|c| {
                if let Command::Frame(bytes) = c {
                    Some(bytes)
                } else {
                    None
                }
            })
            .collect();
        assert!(frames.iter().all(|f| f.len() == 960));
        // Resampling has edge transients; stable center samples identify each
        // audible reply and detect replay of the preceding generation.
        for (index, frame) in frames.iter().enumerate() {
            let sample = i16::from_le_bytes([frame[480], frame[481]]);
            let expected = if index < 12 { 5000 } else { -5000 };
            assert!((i32::from(sample) - expected).abs() <= 10);
        }
    }

    async fn settle(
        work: &mut Pin<Box<impl Future<Output = Result<super::super::Stats, Error>>>>,
        observer: &Observer,
        receipts: &Cell<usize>,
        number: usize,
    ) {
        for _ in 0..50 {
            poll(work.as_mut()).await;
            if observer
                .commands()
                .iter()
                .filter(|c| matches!(c, Command::Stop))
                .count()
                == number
            {
                break;
            }
            tokio::time::advance(Duration::from_millis(20)).await;
        }
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        observer.finish(Ok(()));
        for _ in 0..50 {
            poll(work.as_mut()).await;
            if receipts.get() == number {
                return;
            }
            tokio::time::advance(Duration::from_millis(20)).await;
        }
        assert_eq!(receipts.get(), number);
    }

    async fn idle_noise(tx: &mpsc::Sender<MediaEvent>) {
        tx.send(MediaEvent::Audio(Box::new([7000; 320]))).await.unwrap();
        tx.send(MediaEvent::Control(Event::ReplyDone { text: "stale".into() }))
            .await
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn voice_listen_cancellation_during_idle_never_reports_successful_end() {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        observer.wake("Computer", vec![]);
        let Some(LinkEvent::Wake(microphone)) = events.recv().await.unwrap() else {
            unreachable!()
        };
        let (tx, rx) = mpsc::channel(64);
        let mut port = Port(rx);
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let receipts = Cell::new(0);
        let mut work = Box::pin(super::super::run(
            &client,
            &mut events,
            microphone,
            &mut port,
            Options {
                listen_timeout: Some(Duration::from_secs(3)),
                reply_lead_ms: 200,
                response_deadline: Instant::now() + Duration::from_secs(10),
                played_timeout: Duration::from_secs(1),
                muted: false,
            },
            async {
                cancelled.await.unwrap();
            },
            |notice| {
                if matches!(notice, Notice::ReplyPlayed) {
                    receipts.set(receipts.get() + 1);
                }
                Ok(())
            },
        ));
        reply(&tx, 5000).await;
        settle(&mut work, &observer, &receipts, 1).await;
        cancel.send(()).unwrap();
        assert_eq!(work.await, Err(Error::Control));
        assert_eq!(receipts.get(), 1);
        assert!(!observer.commands().contains(&Command::EndRun));
    }
}
