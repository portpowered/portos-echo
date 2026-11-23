use super::{RuntimeCompletion, RuntimeOptions, execute_runtime};
use crate::{Error, bridge_settings::SettingsService, bridge_time::Watch, bridge_trace::Trace};
use echo_link::LinkConnector;
use echo_media::MediaPeer;
use echo_screen_server::SettingsRequest;
use std::future::Future;
use tokio::sync::mpsc;

pub(in crate::bridge_voice) async fn execute_owned<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    settings: Option<(SettingsService, mpsc::Sender<SettingsRequest>)>,
    (trace, time): (Trace, Option<Watch>),
) -> Result<RuntimeCompletion, Error> {
    crate::bridge_trace::tick_fixture::attach(&trace);
    let result = Box::pin(execute_runtime(
        options,
        connector,
        peer,
        cancelled,
        settings,
        trace.clone(),
    ))
    .await;
    if let Some(time) = time {
        time.stop().await;
    }
    let drain = trace.finish();
    result.and_then(|completion| drain.map(|()| completion))
}
