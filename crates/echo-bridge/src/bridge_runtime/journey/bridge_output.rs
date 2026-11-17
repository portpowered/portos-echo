//! Retain reply admission across owner turns instead of awaiting it exclusively.
use super::{EchoLink, Error, FrameQueue, Future, LeadPacer, Pin, PlaybackContext, ReplySink, json};

type Admission<'a> = Pin<Box<dyn Future<Output = Result<(), echo_link::Error>> + Send + 'a>>;

pub(super) struct Output<'a, L: EchoLink> {
    sink: &'a L::ReplySink,
    pending: Option<(Admission<'a>, u64)>,
}

impl<'a, L: EchoLink> Output<'a, L> {
    pub(super) fn new(sink: &'a L::ReplySink) -> Self {
        Self { sink, pending: None }
    }
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_none()
    }
    pub(super) fn discard(&mut self) -> u64 {
        // An unadmitted frame belongs to the bridge, not the transport receipt.
        u64::from(self.pending.take().is_some())
    }
    pub(super) async fn next(
        &mut self,
        pacer: &mut LeadPacer,
        queue: &mut FrameQueue<Vec<u8>>,
        sent: u64,
    ) -> Result<Option<u64>, Error> {
        if let Some((receipt, sent)) = &mut self.pending {
            receipt.await.map_err(|_| Error::Wyoming)?;
            let sent = *sent;
            self.pending = None;
            return Ok(Some(sent));
        }
        if queue.is_empty() {
            return std::future::pending().await;
        }
        if let Some(frame) = pacer.next_frame(queue).await {
            self.pending = Some((Box::pin(self.sink.send(frame)), sent));
        }
        Ok(None)
    }
}

impl<L: EchoLink> PlaybackContext<'_, L> {
    pub(super) fn admitted(&mut self, sent: u64) -> Result<(), Error> {
        if !self.speaking {
            self.microphone_at_reply = Some(sent);
            self.trace.record("reply.started", Some(self.id), json!({}))?;
            self.screen.publish(echo_screen_contract::UiState::Speaking)?;
            self.speaking = true;
        }
        Ok(())
    }
}
