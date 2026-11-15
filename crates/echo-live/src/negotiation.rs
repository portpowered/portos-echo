use crate::Error;
use str0m::Rtc;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{Direction, MediaKind, Mid};

#[derive(Default)]
pub(crate) struct Negotiation {
    pending: Option<SdpPendingOffer>,
    negotiated: bool,
    pub mid: Option<Mid>,
}

impl Negotiation {
    pub fn offer(&mut self, rtc: &mut Rtc) -> Result<String, Error> {
        self.check_new()?;
        let mut change = rtc.sdp_api();
        self.mid = Some(change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None));
        change.add_channel("oai-events".into());
        let (offer, pending) = change.apply().ok_or(Error::State)?;
        self.pending = Some(pending);
        Ok(offer.to_sdp_string())
    }

    /// Accept an offer from the signaling boundary.
    /// # Errors
    /// Returns typed negotiation errors without including SDP diagnostics.
    pub fn answer(&mut self, rtc: &mut Rtc, offer: &str) -> Result<String, Error> {
        self.check_new()?;
        let offer = SdpOffer::from_sdp_string(offer).map_err(|_| Error::InvalidSdp)?;
        let answer = rtc.sdp_api().accept_offer(offer).map_err(|_| Error::InvalidSdp)?;
        self.negotiated = true;
        Ok(answer.to_sdp_string())
    }

    /// Apply the answer to this peer's outstanding offer.
    /// # Errors
    /// Returns `Closed`, `State` or `InvalidSdp` without echoing input.
    pub fn apply_answer(&mut self, rtc: &mut Rtc, answer: &str) -> Result<(), Error> {
        if self.pending.is_none() {
            return Err(Error::State);
        }
        let answer = SdpAnswer::from_sdp_string(answer).map_err(|_| Error::InvalidSdp)?;
        let pending = self.pending.take().ok_or(Error::State)?;
        rtc.sdp_api()
            .accept_answer(pending, answer)
            .map_err(|_| Error::InvalidSdp)?;
        self.negotiated = true;
        Ok(())
    }

    fn check_new(&self) -> Result<(), Error> {
        if self.negotiated || self.pending.is_some() {
            Err(Error::State)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn invalid_sdp_and_wrong_state_never_connect() -> Result<(), Error> {
        let mut rtc = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .build(Instant::now());
        let mut state = Negotiation::default();
        assert_eq!(state.apply_answer(&mut rtc, "private input"), Err(Error::State));
        assert_eq!(state.answer(&mut rtc, "private input"), Err(Error::InvalidSdp));
        state.offer(&mut rtc)?;
        assert_eq!(state.offer(&mut rtc), Err(Error::State));
        assert_eq!(state.answer(&mut rtc, "private input"), Err(Error::State));
        assert_eq!(state.apply_answer(&mut rtc, "private input"), Err(Error::InvalidSdp));
        assert_eq!(Error::InvalidSdp.to_string(), "invalid SDP");
        assert!(!rtc.is_connected());
        assert!(!state.negotiated);
        Ok(())
    }
}
