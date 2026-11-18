use super::*;
use echo_link::{
    LinkConnector,
    fake::{Command, FakeConnector, Observer},
};
use std::io::Write;

#[test]
fn wake_earcon_waveform_has_bounded_peak_fades_dc_and_rising_tones() {
    let samples = waveform::samples();
    assert_eq!(samples.len(), 3840);
    let peak = samples.iter().map(|sample| sample.unsigned_abs()).max().unwrap();
    let mean = samples.iter().map(|&sample| f64::from(sample)).sum::<f64>() / 3840.0;
    let rms = energy(&samples).sqrt();
    assert!(peak <= 983);
    assert!(peak >= 970);
    assert!(rms.is_finite() && rms > 500.0 && rms < 700.0);
    assert!(mean.abs() <= 1.0);
    for tone in samples.as_chunks::<1920>().0 {
        assert_eq!((tone[0], tone[1919]), (0, 0));
        assert!(energy(&tone[..48]) < energy(&tone[480..1440]) / 100.0);
        assert!(energy(&tone[1872..]) < energy(&tone[480..1440]) / 100.0);
    }
    let crossings = |tone: &[i16]| tone.windows(2).filter(|pair| pair[0] <= 0 && pair[1] > 0).count();
    assert_eq!(crossings(&samples[..1920]), 60);
    assert_eq!(crossings(&samples[1920..]), 80);
    writeln!(std::io::stdout(),
        "wake_earcon waveform: samples=3840 rate=24000 duration_ms=160 peak={peak} rms={rms:.3} mean={mean:.6} crossings=60,80"
    ).unwrap();
}

fn energy(samples: &[i16]) -> f64 {
    samples.iter().map(|&sample| f64::from(sample).powi(2)).sum::<f64>()
        / f64::from(u32::try_from(samples.len()).unwrap())
}

async fn receipt_reader(events: &mut impl LinkEvents, done: &tokio::sync::Notify) {
    loop {
        tokio::select! {
            biased;
            duration = completed() => {
                assert!(duration <= Duration::from_millis(160));
                done.notify_one();
                return;
            }
            event = events.recv() => {
                assert!(receipt(&event.unwrap().unwrap()));
            }
        }
    }
}

async fn acknowledge_cut(observer: &Observer) {
    loop {
        if observer
            .commands()
            .iter()
            .any(|command| matches!(command, Command::Cut(_)))
        {
            observer.finish(Ok(()));
            return;
        }
        // Yield is a readiness poll of the explicit memory fake, not a timer.
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn wake_earcon_full_waveform_waits_for_playout_and_own_receipt() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let done = tokio::sync::Notify::new();
    let work = async {
        start().unwrap();
        receipt_reader(&mut events, &done).await;
        assert!(!active());
        Ok(())
    };
    let remote = async {
        // Explicitly advance the clock only after each paced frame is observed.
        for count in 1..=8 {
            while observer
                .commands()
                .iter()
                .filter(|command| matches!(command, Command::Frame(_)))
                .count()
                < count
            {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(FRAME).await;
        }
        assert!(
            !observer
                .commands()
                .iter()
                .any(|command| matches!(command, Command::Cut(_)))
        );
        tokio::time::advance(OUTPUT_DELAY).await;
        acknowledge_cut(&observer).await;
        done.notified().await;
    };
    let (result, ()) = tokio::join!(scope(&client, true, work), remote);
    assert_eq!(result, Ok(()));
    let pcm: Vec<u8> = observer
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            Command::Frame(frame) => Some(frame),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(pcm.len(), 7680);
    assert_eq!(
        pcm,
        waveform::samples()
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>()
    );
}

#[tokio::test(start_paused = true)]
async fn wake_earcon_disabled_has_no_speaker_operation_or_success() {
    let observer = Observer::default();
    let client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    scope(&client, false, async {
        start()?;
        assert!(!active());
        preempt();
        idle().await
    })
    .await
    .unwrap();
    assert_eq!(observer.commands(), [Command::Connect]);
}

#[tokio::test(start_paused = true)]
async fn wake_earcon_missing_played_fails_with_no_success_and_bounded_cleanup() {
    let observer = Observer::default();
    let client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let began = Instant::now();
    let result = scope(&client, true, async {
        start()?;
        failed().await;
        assert!(current().unwrap().state().completed.is_none());
        Err(Error::Control)
    })
    .await;
    assert_eq!(result, Err(Error::Control));
    assert!(began.elapsed() <= SETTLE + OUTPUT_DELAY);
    client.close().await.unwrap();
    assert_eq!(observer.commands().last(), Some(&Command::Close));
}

#[tokio::test(start_paused = true)]
async fn wake_earcon_cancellation_before_admission_has_no_chime() {
    let observer = Observer::default();
    let client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    assert_eq!(
        scope(&client, true, async { Err(Error::Control) }).await,
        Err(Error::Control)
    );
    assert_eq!(observer.commands(), [Command::Connect]);
}

#[tokio::test(start_paused = true)]
async fn wake_earcon_preempt_discards_unsent_frames_and_settles_before_idle() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let done = tokio::sync::Notify::new();
    let work = async {
        start()?;
        receipt_reader(&mut events, &done).await;
        idle().await?;
        Ok(())
    };
    let remote = async {
        while observer
            .commands()
            .iter()
            .filter(|command| matches!(command, Command::Frame(_)))
            .count()
            < 2
        {
            tokio::task::yield_now().await;
            tokio::time::advance(FRAME).await;
        }
        preempt();
        acknowledge_cut(&observer).await;
        done.notified().await;
    };
    // The remote shares the scoped context, as the real reply producer does.
    scope(&client, true, async {
        let (result, ()) = tokio::join!(work, remote);
        result
    })
    .await
    .unwrap();
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|command| matches!(command, Command::Frame(_)))
            .count(),
        2
    );
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|command| matches!(command, Command::Cut(_)))
            .count(),
        1
    );
}
