use crate::Error;
use echo_contracts::wyoming::{Decoder, Event, Header, WyomingEvent, encode};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// One bounded wire frame is accumulated before feeding the shared decoder.
/// Byte-wise reads here use a buffered reader at the session boundary.
pub(crate) async fn read<R: AsyncRead + Unpin>(reader: &mut R, limit: usize) -> Result<Event, Error> {
    let mut bytes = Vec::new();
    loop {
        let byte = reader.read_u8().await.map_err(|_| {
            if bytes.is_empty() {
                Error::Disconnected
            } else {
                Error::Protocol
            }
        })?;
        bytes.push(byte);
        if bytes.len() > limit {
            return Err(Error::Protocol);
        }
        if byte == b'\n' {
            break;
        }
    }
    let header: Header = serde_json::from_slice(&bytes[..bytes.len() - 1]).map_err(|_| Error::Protocol)?;
    let total = bytes
        .len()
        .checked_add(header.data_length)
        .and_then(|n| n.checked_add(header.payload_length))
        .ok_or(Error::Protocol)?;
    if total > limit {
        return Err(Error::Protocol);
    }
    let start = bytes.len();
    bytes.resize(total, 0);
    reader
        .read_exact(&mut bytes[start..])
        .await
        .map_err(|_| Error::Protocol)?;
    Decoder::new()
        .feed(&bytes)
        .map_err(|_| Error::Protocol)?
        .pop()
        .ok_or(Error::Protocol)
}

pub(crate) async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    event: &WyomingEvent,
    payload: Vec<u8>,
    limit: usize,
) -> Result<(), Error> {
    if payload.len() > limit {
        return Err(Error::Capacity);
    }
    let value = serde_json::to_value(event).map_err(|_| Error::Protocol)?;
    let kind: String = value["type"].as_str().ok_or(Error::Protocol)?.into();
    let data = value["data"].clone();
    // hassmic reads run-satellite's fields only from a separate data segment:
    // inline, its capture watchdog silently stayed at the 30 s default.
    let (inline, segment) = if kind == "run-satellite" && data.is_object() {
        (None, Some(data))
    } else {
        (Some(data), None)
    };
    let event = Event {
        header: Header {
            r#type: kind,
            data: inline,
            data_length: 0,
            payload_length: 0,
        },
        data: segment,
        payload,
    };
    let bytes = encode(&event);
    if bytes.len() > limit {
        return Err(Error::Capacity);
    }
    writer.write_all(&bytes).await.map_err(|_| Error::Disconnected)
}
