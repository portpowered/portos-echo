//! Capture completion is independent of the retained remote's transport work.
use super::Control;
use tokio::sync::mpsc;

pub(in crate::lab_run::sim::m2::codex) async fn advance(
    work: impl std::future::Future<Output = Result<usize, String>>,
    commands: &mut mpsc::Receiver<Control>,
    capture: &mut Option<usize>,
    servicing: &mut bool,
    retained: bool,
) -> Result<Option<bool>, String> {
    tokio::pin!(work);
    #[cfg(test)]
    let mut maintenance = None;
    loop {
        tokio::select! {
            received = &mut work, if *servicing => {
                let received = received?;
                #[cfg(test)]
                if let Some(done) = maintenance.take() {
                    let _: Result<(), ()> = tokio::sync::oneshot::Sender::send(done, ());
                }
                let completed = if let Some(count) = capture.as_mut() {
                    *count += received;
                    *count >= 12
                } else { false };
                if completed {
                    *capture = None;
                    // M1 deliberately ends the call after every capture. A
                    // retained call must keep servicing its peer between wakes.
                    *servicing = retained;
                }
                return Ok(Some(completed));
            },
            command = commands.recv() => match command {
                Some(Control::Quiesce(done)) => { *capture = None; *servicing = false; let _ = done.send(()); },
                Some(Control::Close) | None => return Ok(None),
                #[cfg(test)]
                Some(Control::NextCapture) => { *capture = Some(0); *servicing = true; },
                #[cfg(test)]
                Some(Control::Maintenance(done)) => maintenance = Some(done),
                Some(Control::Transcripts | Control::Tail | Control::Reply) =>
                    return Err("m1: automatic transcript ownership".into()),
            },
        }
    }
}

#[cfg(test)]
pub(in crate::lab_run::sim::m2::codex) fn report(message: std::fmt::Arguments<'_>) {
    use std::io::Write;
    writeln!(std::io::stdout(), "{message}").unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::{Future, ready},
        task::Poll,
    };
    use tokio::sync::oneshot;

    async fn pending_once(future: std::pin::Pin<&mut impl Future>) {
        let mut future = future;
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn capture_pause_preserves_transport_errors_and_rejects_foreign_controls() {
        let (commands, mut receive) = mpsc::channel(2);
        let mut capture = None;
        let mut servicing = true;
        assert_eq!(
            advance(
                ready(Err("real IO failure".into())),
                &mut receive,
                &mut capture,
                &mut servicing,
                true
            )
            .await,
            Err("real IO failure".into())
        );
        for control in [Control::Transcripts, Control::Tail, Control::Reply] {
            commands.send(control).await.unwrap();
            assert_eq!(
                advance(std::future::pending(), &mut receive, &mut capture, &mut servicing, true).await,
                Err("m1: automatic transcript ownership".into())
            );
        }
    }

    #[tokio::test]
    async fn explicit_quiesce_and_fresh_call_completion_stop_transport_work() {
        let (commands, mut receive) = mpsc::channel(2);
        let mut capture = Some(0);
        let mut servicing = true;
        assert_eq!(
            advance(ready(Ok(12)), &mut receive, &mut capture, &mut servicing, false).await,
            Ok(Some(true))
        );
        assert!(!servicing, "M1's per-capture call is ending");
        servicing = true;
        let (send, done) = oneshot::channel();
        commands.send(Control::Quiesce(send)).await.unwrap();
        let mut driver = Box::pin(advance(
            std::future::pending(),
            &mut receive,
            &mut capture,
            &mut servicing,
            true,
        ));
        pending_once(driver.as_mut()).await;
        done.await.unwrap();
        commands.send(Control::Close).await.unwrap();
        assert_eq!(driver.await, Ok(None));
        assert!(!servicing);
        assert_eq!(capture, None);
    }

    #[tokio::test(start_paused = true)]
    async fn capture_pause_services_maintenance_then_reuses_until_own_close() {
        let (commands, mut receive) = mpsc::channel(2);
        let mut capture = Some(0);
        let mut servicing = true;
        assert_eq!(
            advance(ready(Ok(12)), &mut receive, &mut capture, &mut servicing, true).await,
            Ok(Some(true))
        );
        assert_eq!(capture, None);
        let (maintain, maintained) = oneshot::channel();
        let mut driver = Box::pin(advance(
            async {
                maintained.await.unwrap();
                Ok(7)
            },
            &mut receive,
            &mut capture,
            &mut servicing,
            true,
        ));
        pending_once(driver.as_mut()).await;
        maintain.send(()).unwrap();
        std::future::poll_fn(|cx| {
            assert_eq!(
                driver.as_mut().poll(cx),
                Poll::Ready(Ok(Some(false))),
                "retained peer maintenance stalled after capture completion"
            );
            Poll::Ready(())
        })
        .await;
        drop(driver);
        assert_eq!(capture, None, "maintenance must not admit another capture");
        commands.send(Control::NextCapture).await.unwrap();
        let mut driver = Box::pin(advance(
            std::future::pending(),
            &mut receive,
            &mut capture,
            &mut servicing,
            true,
        ));
        pending_once(driver.as_mut()).await;
        drop(driver);
        assert_eq!(capture, Some(0));
        assert_eq!(
            advance(ready(Ok(12)), &mut receive, &mut capture, &mut servicing, true).await,
            Ok(Some(true))
        );
        commands.send(Control::Close).await.unwrap();
        assert_eq!(
            advance(std::future::pending(), &mut receive, &mut capture, &mut servicing, true).await,
            Ok(None)
        );
        report(format_args!(
            "F-3: capture completions=2; paused maintenance=1; extra captures=0; own close=1"
        ));
    }
}
