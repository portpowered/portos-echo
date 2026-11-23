use crate::{ClientOptions, Error};
use echo_contracts::wyoming::{AudioChunkData, Decoder, Event, Header, WyomingEvent, encode};
use echo_link::{MicPayload, MicPool};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const CAPTURE_BYTES: usize = 8192;
const HEADER_BYTES: usize = 512;

#[derive(Debug)]
pub(crate) enum Frame {
    Audio(AudioChunkData, MicPayload),
    Control(Event),
}

impl Frame {
    pub(crate) fn pipeline_frame(&self) -> bool {
        match self {
            Self::Audio(..) => true,
            Self::Control(frame) => matches!(
                frame.header.r#type.as_str(),
                "run-pipeline" | "audio-start" | "audio-stop"
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioHeader<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(rename = "version")]
    _version: Option<&'a str>,
    #[serde(default)]
    data_length: usize,
    #[serde(default)]
    payload_length: usize,
    data: Option<&'a RawValue>,
}

// The authored JSON encoder sorts map keys. Use the same order when no
// extension keys need merging into the map.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AudioFields {
    channels: u16,
    rate: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<u64>,
    width: u16,
}

impl From<AudioFields> for AudioChunkData {
    fn from(fields: AudioFields) -> Self {
        Self {
            rate: fields.rate,
            width: fields.width,
            channels: fields.channels,
            timestamp: fields.timestamp,
            extra: BTreeMap::new(),
        }
    }
}

pub(crate) struct Reader {
    bytes: Vec<u8>,
    mic: MicPool,
    limit: usize,
}

impl Reader {
    pub(crate) fn new(limit: usize, mic_capacity: usize) -> Self {
        let slots = mic_capacity.min(ClientOptions::default().mic_capacity);
        Self {
            bytes: Vec::with_capacity(limit.min(CAPTURE_BYTES + HEADER_BYTES)),
            // One channel entry and one reader waiting to send it.
            mic: MicPool::new(slots + 2, limit.min(CAPTURE_BYTES)),
            limit,
        }
    }

    pub(crate) async fn read(&mut self, reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Frame, Error> {
        self.read_header(reader).await?;
        let start = self.bytes.len();
        let line = &self.bytes[..start - 1];
        let (data_len, payload_len, audio) = lengths(line)?;
        let data_end = start.checked_add(data_len).ok_or(Error::Protocol)?;
        let end = data_end.checked_add(payload_len).ok_or(Error::Protocol)?;
        if end > self.limit {
            return Err(Error::Protocol);
        }
        self.bytes.resize(end, 0);
        reader
            .read_exact(&mut self.bytes[start..])
            .await
            .map_err(|_| Error::Protocol)?;
        let fields = match audio {
            AudioData::Inline(fields) => Some(fields),
            AudioData::Segment => audio_fields(&self.bytes[start..data_end]),
            AudioData::Fallback => None,
        };
        if let Some(fields) = fields {
            return Ok(Frame::Audio(fields.into(), self.mic.copy(&self.bytes[data_end..])));
        }
        let frame = Decoder::new()
            .feed(&self.bytes)
            .map_err(|_| Error::Protocol)?
            .pop()
            .ok_or(Error::Protocol)?;
        if frame.header.r#type == "audio-chunk" {
            let WyomingEvent::AudioChunk(data) = frame.typed().map_err(|_| Error::Protocol)? else {
                return Err(Error::Protocol);
            };
            return Ok(Frame::Audio(data, frame.payload.into()));
        }
        Ok(Frame::Control(frame))
    }

    async fn read_header(&mut self, reader: &mut (impl AsyncBufRead + Unpin)) -> Result<(), Error> {
        self.bytes.clear();
        loop {
            let available = reader.fill_buf().await.map_err(|_| self.read_error())?;
            if available.is_empty() {
                return Err(self.read_error());
            }
            let newline = available.iter().position(|&byte| byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index + 1);
            if consumed > self.limit - self.bytes.len() {
                return Err(Error::Protocol);
            }
            self.bytes.extend_from_slice(&available[..consumed]);
            reader.consume(consumed);
            if newline.is_some() {
                return Ok(());
            }
        }
    }

    fn read_error(&self) -> Error {
        if self.bytes.is_empty() {
            Error::Disconnected
        } else {
            Error::Protocol
        }
    }
}

enum AudioData {
    Inline(AudioFields),
    Segment,
    Fallback,
}

fn lengths(line: &[u8]) -> Result<(usize, usize, AudioData), Error> {
    if let Some(header) = audio_header(line) {
        let audio = match (header.data_length, header.data) {
            (0, Some(data)) => audio_fields(data.get().as_bytes()).map_or(AudioData::Fallback, AudioData::Inline),
            (1.., None) => AudioData::Segment,
            _ => AudioData::Fallback,
        };
        return Ok((header.data_length, header.payload_length, audio));
    }
    let header: Header = serde_json::from_slice(line).map_err(|_| Error::Protocol)?;
    Ok((header.data_length, header.payload_length, AudioData::Fallback))
}

fn audio_header(line: &[u8]) -> Option<AudioHeader<'_>> {
    if line.iter().copied().find(|byte| !byte.is_ascii_whitespace())? != b'{' {
        return None;
    }
    let header: AudioHeader<'_> = serde_json::from_slice(line).ok()?;
    (header.kind == "audio-chunk").then_some(header)
}

fn audio_fields(data: &[u8]) -> Option<AudioFields> {
    if data.iter().copied().find(|byte| !byte.is_ascii_whitespace())? != b'{' {
        return None;
    }
    serde_json::from_slice(data).ok()
}

pub(crate) struct Writer(Vec<u8>);

impl Writer {
    pub(crate) fn new() -> Self {
        Self(Vec::with_capacity(HEADER_BYTES + 960))
    }

    pub(crate) async fn write(
        &mut self,
        writer: &mut (impl AsyncWrite + Unpin),
        event: &WyomingEvent,
        payload: Vec<u8>,
        limit: usize,
    ) -> Result<(), Error> {
        if payload.len() > limit {
            return Err(Error::Capacity);
        }
        if let WyomingEvent::AudioChunk(data) = event
            && data.extra.is_empty()
        {
            self.audio_header(data, payload.len())?;
            if self.0.len() > limit - payload.len() {
                return Err(Error::Capacity);
            }
            self.0.extend_from_slice(&payload);
            return writer.write_all(&self.0).await.map_err(|_| Error::Disconnected);
        }
        let bytes = encode_control(event, payload)?;
        if bytes.len() > limit {
            return Err(Error::Capacity);
        }
        writer.write_all(&bytes).await.map_err(|_| Error::Disconnected)
    }

    fn audio_header(&mut self, data: &AudioChunkData, payload_length: usize) -> Result<(), Error> {
        #[derive(Serialize)]
        struct ChunkHeader {
            data: AudioFields,
            data_length: usize,
            payload_length: usize,
            #[serde(rename = "type")]
            kind: &'static str,
        }

        self.0.clear();
        serde_json::to_writer(
            &mut self.0,
            &ChunkHeader {
                data: AudioFields {
                    channels: data.channels,
                    rate: data.rate,
                    timestamp: data.timestamp,
                    width: data.width,
                },
                data_length: 0,
                payload_length,
                kind: "audio-chunk",
            },
        )
        .map_err(|_| Error::Protocol)?;
        self.0.push(b'\n');
        Ok(())
    }
}

fn encode_control(event: &WyomingEvent, payload: Vec<u8>) -> Result<Vec<u8>, Error> {
    let value = serde_json::to_value(event).map_err(|_| Error::Protocol)?;
    let kind: String = value["type"].as_str().ok_or(Error::Protocol)?.into();
    let data = value["data"].clone();
    // hassmic reads run-satellite's fields only from a separate data segment.
    let (inline, segment) = if kind == "run-satellite" && data.is_object() {
        (None, Some(data))
    } else {
        (Some(data), None)
    };
    Ok(encode(&Event {
        header: Header {
            r#type: kind,
            data: inline,
            data_length: 0,
            payload_length: 0,
        },
        data: segment,
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{BufReader, duplex};

    #[tokio::test]
    async fn fragmented_audio_preserves_segment_precedence_and_large_valid_payloads() {
        let (mut peer, socket) = duplex(1024);
        let mut reader = Reader::new(16_384, 1);
        let mut socket = BufReader::new(socket);
        let data = br#"{"rate":16000,"width":2,"channels":1,"timestamp":21}"#;
        let header = format!(
            "{{\"type\":\"audio-chunk\",\"version\":\"1.5.4\",\"data_length\":{},\"payload_length\":8192}}\n",
            data.len(),
        );
        let mut unusual = crate::wyoming_test_support::frame(
            "audio-chunk",
            json!({"rate":24000,"width":2,"channels":1,"timestamp":7,"custom":"inline"}),
            vec![3; 8194],
        );
        unusual.data = Some(json!({"rate":16000,"timestamp":31,"custom":"segment"}));
        let expected = unusual.typed().unwrap();
        let raw = encode(&unusual);
        let send = async move {
            peer.write_all(header.as_bytes()).await.unwrap();
            peer.write_all(data).await.unwrap();
            peer.write_all(&[1; 8192]).await.unwrap();
            for part in raw.chunks(23) {
                peer.write_all(part).await.unwrap();
            }
        };
        let read = async {
            let Frame::Audio(data, held) = reader.read(&mut socket).await.unwrap() else {
                unreachable!("missing audio")
            };
            assert_eq!(
                (data.rate, data.width, data.channels, data.timestamp),
                (16000, 2, 1, Some(21))
            );
            assert_eq!(&*held, &[1; 8192]);
            let Frame::Audio(data, payload) = reader.read(&mut socket).await.unwrap() else {
                unreachable!("missing audio")
            };
            assert_eq!(WyomingEvent::AudioChunk(data), expected);
            assert_eq!(&*payload, &[3; 8194]);
            assert_eq!(&*held, &[1; 8192]);
            assert!(matches!(reader.read(&mut socket).await, Err(Error::Disconnected)));
        };
        tokio::join!(send, read);
    }

    #[tokio::test]
    async fn reusable_writer_matches_canonical_chunk_and_rejects_before_writing() {
        let (mut socket, mut peer) = duplex(4096);
        let mut writer = Writer::new();
        for timestamp in [None, Some(u64::MAX)] {
            let chunk = AudioChunkData {
                rate: 24_000,
                channels: 1,
                width: 2,
                timestamp,
                extra: BTreeMap::new(),
            };
            let payload = vec![4; 960];
            let data = serde_json::to_value(&chunk).unwrap();
            let expected = encode(&crate::wyoming_test_support::frame(
                "audio-chunk",
                data,
                payload.clone(),
            ));
            let event = WyomingEvent::AudioChunk(chunk);
            assert_eq!(
                writer
                    .write(&mut socket, &event, payload.clone(), expected.len() - 1)
                    .await,
                Err(Error::Capacity)
            );
            writer
                .write(&mut socket, &event, payload, expected.len())
                .await
                .unwrap();
            let mut actual = vec![0; expected.len()];
            peer.read_exact(&mut actual).await.unwrap();
            assert_eq!(actual, expected);
        }
        drop(socket);
        assert_eq!(peer.read(&mut [0; 1]).await.unwrap(), 0);
    }
}
