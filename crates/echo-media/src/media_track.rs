// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host audio_track.rs; Modified by Port OS.
use crate::Error;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use std::{sync::Arc, time::Duration};
use webrtc::{
    media_stream::{MediaStreamTrack, track_local::static_sample::TrackLocalStaticSample},
    peer_connection::MediaEngine,
};

pub(crate) struct AudioTrack {
    pub(crate) track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
}
impl AudioTrack {
    pub(crate) fn new() -> Result<(MediaEngine, Self), Error> {
        let ssrc = rand::random();
        let codec = RTCRtpCodec {
            mime_type: "audio/opus".into(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".into(),
            rtcp_feedback: vec![],
        };
        let mut media = MediaEngine::default();
        media
            .register_codec(
                RTCRtpCodecParameters {
                    rtp_codec: codec.clone(),
                    payload_type: 111,
                },
                RtpCodecKind::Audio,
            )
            .map_err(|_| Error::Audio)?;
        let track = TrackLocalStaticSample::new(
            tokio::time::Instant::now().into_std(),
            MediaStreamTrack::new(
                "realtime".into(),
                format!("audio-{ssrc}"),
                "microphone".into(),
                RtpCodecKind::Audio,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec,
                    active: true,
                    ..Default::default()
                }],
            ),
        )
        .map_err(|_| Error::Audio)?;
        Ok((
            media,
            Self {
                track: Arc::new(track),
                ssrc,
            },
        ))
    }
    pub(crate) async fn send(&self, data: Vec<u8>) -> Result<(), Error> {
        tokio::time::timeout(
            Duration::from_millis(100),
            self.track.write_sample(
                self.ssrc,
                111,
                &rtc::media::Sample {
                    data: data.into(),
                    duration: Duration::from_millis(20),
                    ..rtc::media::Sample::new(tokio::time::Instant::now().into_std())
                },
                &[],
            ),
        )
        .await
        .map_err(|_| Error::Deadline)?
        .map_err(|_| Error::Transport)
    }
}
