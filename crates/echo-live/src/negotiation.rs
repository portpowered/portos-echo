use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::MediaEngine;
use rtc::peer_connection::configuration::setting_engine::SettingEngineBuilder;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::{CandidateConfig, CandidateHostConfig, RTCIceCandidate, RTCIceCandidateInit};
use rtc::peer_connection::{RTCPeerConnection, RTCPeerConnectionBuilder};
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};

use rtc::rtp_transceiver::RTCRtpSenderId;

use crate::{AudioFrame, Error};

pub(crate) fn connection(address: SocketAddr, now: Instant) -> Result<RTCPeerConnection, Error> {
    let mut media = MediaEngine::default();
    media
        .register_codec(
            RTCRtpCodecParameters {
                rtp_codec: codec(),
                payload_type: 111,
            },
            RtpCodecKind::Audio,
        )
        .map_err(|_| Error::Protocol)?;
    let mut rtc = RTCPeerConnectionBuilder::new()
        .with_media_engine(media)
        // The previous peer expires a path after unanswered checks, not a
        // single delayed caller tick. The adapter retains that bounded probe
        // policy; rtc continues sending its normal consent checks.
        .with_setting_engine(
            SettingEngineBuilder::new()
                .with_ice_timeouts(None, Some(Duration::ZERO), None)
                .build(),
        )
        .build(now)
        .map_err(|_| Error::Protocol)?;
    let candidate = CandidateHostConfig {
        base_config: CandidateConfig {
            network: "udp".into(),
            address: address.ip().to_string(),
            port: address.port(),
            component: 1,
            ..Default::default()
        },
        ..Default::default()
    }
    .new_candidate_host()
    .map_err(|_| Error::Protocol)?;
    rtc.add_local_candidate(
        RTCIceCandidate::from(&candidate)
            .to_json()
            .map_err(|_| Error::Protocol)?,
    )
    .map_err(|_| Error::Protocol)?;
    rtc.add_local_candidate(RTCIceCandidateInit::default())
        .map_err(|_| Error::Protocol)?;
    Ok(rtc)
}

fn codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: "audio/opus".into(),
        clock_rate: 48_000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".into(),
        rtcp_feedback: vec![],
    }
}

// Direction applies to RTP media. rtc also emits it for the SCTP m-line;
// omit that redundant attribute to retain the existing signaling shape.
fn local_sdp(rtc: &RTCPeerConnection) -> Result<String, Error> {
    let sdp = rtc.local_description().ok_or(Error::Protocol)?.sdp;
    let mut application = false;
    let mut result = String::new();
    for line in sdp.lines() {
        if line.starts_with("m=") {
            application = line.starts_with("m=application ");
        }
        if !(application && line == "a=sendrecv") {
            result.push_str(line);
            result.push_str("\r\n");
        }
    }
    Ok(result)
}

// rtc mutates signaling state before inspecting transport attributes. Reject
// incomplete/conflicting transport descriptions first so a parsed bad SDP
// cannot consume the caller's new/pending negotiation.
fn remote(sdp: &str, answer: bool) -> Result<RTCSessionDescription, Error> {
    let description = if answer {
        RTCSessionDescription::answer(sdp.into())
    } else {
        RTCSessionDescription::offer(sdp.into())
    }
    .map_err(|_| Error::InvalidSdp)?;
    let parsed = description.unmarshal().map_err(|_| Error::InvalidSdp)?;
    if parsed.media_descriptions.is_empty() {
        return Err(Error::InvalidSdp);
    }
    let mut transport = [None, None, None];
    for (i, key) in ["ice-ufrag", "ice-pwd", "fingerprint"].into_iter().enumerate() {
        transport[i] = parsed.attribute(key).cloned();
    }
    for media in &parsed.media_descriptions {
        if media.attribute("mid").flatten().is_none_or(str::is_empty) {
            return Err(Error::InvalidSdp);
        }
        for (i, key) in ["ice-ufrag", "ice-pwd", "fingerprint"].into_iter().enumerate() {
            if let Some(value) = media.attribute(key).flatten() {
                if transport[i].as_ref().is_some_and(|prior| prior != value) {
                    return Err(Error::InvalidSdp);
                }
                transport[i] = Some(value.into());
            }
        }
        for attr in media.attributes.iter().filter(|a| a.key == "candidate") {
            rtc::ice::candidate::unmarshal_candidate(attr.value.as_deref().ok_or(Error::InvalidSdp)?)
                .map_err(|_| Error::InvalidSdp)?;
        }
    }
    if transport.iter().any(|v| v.as_ref().is_none_or(String::is_empty))
        || transport[2].as_ref().is_none_or(|f| f.split_whitespace().count() != 2)
    {
        return Err(Error::InvalidSdp);
    }
    Ok(description)
}

#[derive(Default)]
pub(crate) struct Negotiation {
    pending: bool,
    negotiated: bool,
    sender: Option<RTCRtpSenderId>,
    ssrc: u32,
}

impl Negotiation {
    fn attach(&mut self, rtc: &mut RTCPeerConnection) -> Result<(), Error> {
        use std::hash::BuildHasher;
        let bytes = std::collections::hash_map::RandomState::new()
            .hash_one("opus")
            .to_le_bytes();
        self.ssrc = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let track = MediaStreamTrack::new(
            "live".into(),
            "opus".into(),
            "audio".into(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(self.ssrc),
                    ..Default::default()
                },
                codec: codec(),
                active: true,
                ..Default::default()
            }],
        );
        self.sender = Some(rtc.add_track(track).map_err(|_| Error::Protocol)?);
        Ok(())
    }

    pub fn offer(&mut self, rtc: &mut RTCPeerConnection, now: Instant) -> Result<String, Error> {
        self.check_new()?;
        self.attach(rtc)?;
        rtc.create_data_channel("oai-events", None)
            .map_err(|_| Error::Protocol)?;
        let offer = rtc.create_offer(None).map_err(|_| Error::Protocol)?;
        rtc.set_local_description(now, offer).map_err(|_| Error::Protocol)?;
        self.pending = true;
        local_sdp(rtc)
    }

    pub fn answer(&mut self, rtc: &mut RTCPeerConnection, now: Instant, sdp: &str) -> Result<String, Error> {
        self.check_new()?;
        let offer = remote(sdp, false)?;
        rtc.set_remote_description(now, offer).map_err(|_| Error::InvalidSdp)?;
        self.attach(rtc)?;
        let answer = rtc.create_answer(None).map_err(|_| Error::InvalidSdp)?;
        rtc.set_local_description(now, answer).map_err(|_| Error::InvalidSdp)?;
        self.negotiated = true;
        local_sdp(rtc)
    }

    pub fn apply_answer(&mut self, rtc: &mut RTCPeerConnection, now: Instant, sdp: &str) -> Result<(), Error> {
        if !self.pending {
            return Err(Error::State);
        }
        let answer = remote(sdp, true)?;
        rtc.set_remote_description(now, answer).map_err(|_| Error::InvalidSdp)?;
        self.pending = false;
        self.negotiated = true;
        Ok(())
    }

    pub fn send_audio(&self, rtc: &mut RTCPeerConnection, now: Instant, frame: AudioFrame) -> Result<(), Error> {
        let mut sender = rtc
            .rtp_sender(self.sender.ok_or(Error::NotReady)?)
            .ok_or(Error::NotReady)?;
        let pt = sender
            .get_parameters()
            .rtp_parameters
            .codecs
            .iter()
            .find(|c| c.rtp_codec.mime_type.eq_ignore_ascii_case("audio/opus"))
            .map(|c| c.payload_type)
            .ok_or(Error::NotReady)?;
        sender
            .write_rtp(
                now,
                rtc::rtp::Packet {
                    header: rtc::rtp::Header {
                        version: 2,
                        payload_type: pt,
                        sequence_number: frame.sequence,
                        timestamp: frame.timestamp,
                        ssrc: self.ssrc,
                        ..Default::default()
                    },
                    payload: frame.payload.into(),
                },
            )
            .map_err(|_| Error::Protocol)
    }

    fn check_new(&self) -> Result<(), Error> {
        if self.negotiated || self.pending {
            Err(Error::State)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::disallowed_methods, reason = "Host time for test")]
    fn invalid_sdp_and_wrong_state_never_connect() -> Result<(), Error> {
        let now = Instant::now();
        let mut rtc = connection("127.0.0.1:12345".parse().unwrap(), now)?;
        let mut state = Negotiation::default();
        assert_eq!(state.apply_answer(&mut rtc, now, "private input"), Err(Error::State));
        assert_eq!(state.answer(&mut rtc, now, "private input"), Err(Error::InvalidSdp));
        state.offer(&mut rtc, now)?;
        assert_eq!(state.offer(&mut rtc, now), Err(Error::State));
        assert_eq!(state.answer(&mut rtc, now, "private input"), Err(Error::State));
        assert_eq!(
            state.apply_answer(&mut rtc, now, "private input"),
            Err(Error::InvalidSdp)
        );
        assert_eq!(Error::InvalidSdp.to_string(), "invalid SDP");
        assert!(!state.negotiated);
        Ok(())
    }
    #[test]
    #[allow(clippy::disallowed_methods, reason = "Host time for test")]
    fn parsed_bad_transport_preserves_new_and_pending_negotiation() -> Result<(), Error> {
        let now = Instant::now();
        let mut a = connection("127.0.0.1:12345".parse().unwrap(), now)?;
        let mut b = connection("127.0.0.1:12346".parse().unwrap(), now)?;
        let mut left = Negotiation::default();
        let mut right = Negotiation::default();
        let offer = left.offer(&mut a, now)?;
        for key in ["mid", "ice-ufrag", "ice-pwd", "fingerprint"] {
            let bad = offer
                .lines()
                .filter(|l| !l.starts_with(&format!("a={key}:")))
                .fold(String::new(), |mut s, l| {
                    s.push_str(l);
                    s.push_str("\r\n");
                    s
                });
            assert_eq!(right.answer(&mut b, now, &bad), Err(Error::InvalidSdp));
            assert!(!right.negotiated);
        }
        let answer = right.answer(&mut b, now, &offer)?;
        for key in ["mid", "ice-ufrag", "ice-pwd", "fingerprint"] {
            let bad =
                answer
                    .lines()
                    .filter(|l| !l.starts_with(&format!("a={key}:")))
                    .fold(String::new(), |mut s, l| {
                        s.push_str(l);
                        s.push_str("\r\n");
                        s
                    });
            assert_eq!(left.apply_answer(&mut a, now, &bad), Err(Error::InvalidSdp));
            assert!(left.pending);
        }
        left.apply_answer(&mut a, now, &answer)?;
        assert!(left.negotiated);
        assert!(!left.pending);
        assert_eq!(left.apply_answer(&mut a, now, &answer), Err(Error::State));
        Ok(())
    }
}
