//! A credential failure latches refusal until explicit sign-in and restart.
use super::bridge_screen::Screen;
use crate::{Error, bridge_health::Health, bridge_trace::Trace};
use serde_json::json;

pub(super) async fn failure(
    error: Error,
    lab: bool,
    health: &Health,
    trace: &mut Trace,
    screen: &Screen,
    id: &str,
) -> Result<(), Error> {
    if let Error::Mcp(reason) = &error {
        trace.record(
            "mcp.run.failed",
            Some(id),
            json!({"reason":crate::bridge_mcp::category(reason)}),
        )?;
        return Ok(());
    }
    health.state.send_modify(|state| state.live = "error");
    // The synthetic lab caller retains its existing fatal-error protocol.
    if lab
        || !matches!(
            error,
            Error::Authorization | Error::Unavailable | Error::Deadline | Error::Decode
        )
    {
        return Err(error);
    }
    let relink = error == Error::Authorization;
    health.state.send_modify(|state| state.relink_required = relink);
    complete_publication(
        screen.publish_confirmed(super::bridge_screen::failure_state(relink)),
        trace,
        id,
        relink,
        error,
    )
    .await
}

async fn complete_publication(
    publication: impl std::future::Future<Output = Result<(), Error>>,
    trace: &mut Trace,
    id: &str,
    relink: bool,
    error: Error,
) -> Result<(), Error> {
    if let Err(error) = publication.await {
        trace.record("run.failed", Some(id), json!({"reason":error.to_string()}))?;
        return Err(error);
    }
    trace.record(
        if relink { "auth.relink_required" } else { "run.failed" },
        Some(id),
        json!({"reason":error.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    #[tokio::test]
    async fn bridge_relink_completion_waits_for_publication() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api-key").unwrap();
        let (release, held) = tokio::sync::oneshot::channel();
        let completion = complete_publication(
            async { held.await.unwrap() },
            &mut trace,
            "run-1",
            true,
            Error::Authorization,
        );
        tokio::pin!(completion);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(completion.as_mut().poll(cx).is_pending())).await);
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        release.send(Ok(())).unwrap();
        assert_eq!(completion.await, Ok(()));
        let entries = std::fs::read_to_string(&path).unwrap();
        assert_eq!(entries.lines().count(), 1);
        let entry: serde_json::Value = serde_json::from_str(&entries).unwrap();
        assert_eq!(entry["event"], "auth.relink_required");
        assert_eq!(entry["reason"], "authorization");
    }

    #[tokio::test]
    async fn bridge_relink_publication_failure_never_claims_relink() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api-key").unwrap();
        assert_eq!(
            complete_publication(
                async { Err(Error::Screen) },
                &mut trace,
                "run-1",
                true,
                Error::Authorization
            )
            .await,
            Err(Error::Screen)
        );
        let entry: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry["event"], "run.failed");
        assert_eq!(entry["reason"], "screen");
        assert_eq!(
            complete_publication(async { Ok(()) }, &mut trace, "run-1", false, Error::Unavailable).await,
            Ok(())
        );
        assert_eq!(
            complete_publication(async { Ok(()) }, &mut trace, "run-1", true, Error::Decode).await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn bridge_relink_trace_failure_is_propagated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api-key").unwrap();
        for publication in [Ok(()), Err(Error::Screen)] {
            assert_eq!(
                complete_publication(
                    async { publication },
                    &mut trace,
                    &"x".repeat(4096),
                    true,
                    Error::Authorization
                )
                .await,
                Err(Error::Trace)
            );
        }
        assert_eq!(std::fs::read(path).unwrap(), b"");
    }

    #[tokio::test]
    async fn bridge_relink_no_screen_and_fatal_branches_keep_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let mut trace = Trace::open(&directory.path().join("trace"), "api-key").unwrap();
        let health = Health::offline();
        let screen = Screen::bind(None).await.unwrap();
        assert_eq!(
            failure(Error::Authorization, true, &health, &mut trace, &screen, "run-1").await,
            Err(Error::Authorization)
        );
        assert_eq!(
            failure(Error::Screen, false, &health, &mut trace, &screen, "run-1").await,
            Err(Error::Screen)
        );
        assert_eq!(
            failure(Error::Authorization, false, &health, &mut trace, &screen, "run-1").await,
            Ok(())
        );
        assert_eq!(
            failure(
                Error::Mcp(echo_mcp_auth::AuthError::RelinkRequired),
                false,
                &health,
                &mut trace,
                &screen,
                "run-1"
            )
            .await,
            Ok(())
        );
        assert!(health.state.borrow().relink_required);
        assert_eq!(health.state.borrow().live, "error");
    }
}
