use super::{Event, Header};

/// Framing failure; after any failure a decoder must be replaced.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("invalid header: {0}")]
    Header(serde_json::Error),
    #[error("invalid external JSON data: {0}")]
    Data(serde_json::Error),
    #[error("frame lengths overflow address space")]
    LengthOverflow,
    #[error("end of input in a partial frame")]
    Truncated,
    #[error("decoder is poisoned by an earlier error")]
    Poisoned,
}

/// Serialize canonical lengths, retaining inline header data separately.
#[must_use]
pub fn encode(event: &Event) -> Vec<u8> {
    let data = event.data.as_ref().map_or_default(serde_json::Value::to_string);
    let mut header = event.header.clone();
    header.data_length = data.len();
    header.payload_length = event.payload.len();
    // Header contains only strings, JSON values and unsigned lengths.
    let mut bytes = serde_json::json!(header).to_string().into_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(data.as_bytes());
    bytes.extend_from_slice(&event.payload);
    bytes
}

/// Incremental transport-independent Wyoming decoder.
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    poisoned: std::cell::Cell<bool>,
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return complete frames in order, retaining any trailing partial frame.
    /// # Errors
    /// Invalid JSON, invalid/overflow lengths, or a previous failure poisons the decoder.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, CodecError> {
        if self.poisoned.get() {
            return Err(CodecError::Poisoned);
        }
        self.buffer.extend_from_slice(bytes);
        let result = self.decode();
        self.poisoned.set(result.is_err());
        result
    }

    fn decode(&mut self) -> Result<Vec<Event>, CodecError> {
        let mut events = Vec::new();
        let mut offset = 0;
        while let Some(newline) = self.buffer[offset..].iter().position(|&b| b == b'\n') {
            let start = offset + newline + 1;
            let header: Header = serde_json::from_slice(&self.buffer[offset..start - 1]).map_err(CodecError::Header)?;
            let data_end = start
                .checked_add(header.data_length)
                .ok_or(CodecError::LengthOverflow)?;
            let end = data_end
                .checked_add(header.payload_length)
                .ok_or(CodecError::LengthOverflow)?;
            if end > self.buffer.len() {
                break;
            }
            let data = if header.data_length == 0 {
                None
            } else {
                Some(serde_json::from_slice(&self.buffer[start..data_end]).map_err(CodecError::Data)?)
            };
            events.push(Event {
                header,
                data,
                payload: self.buffer[data_end..end].to_vec(),
            });
            offset = end;
        }
        self.buffer.drain(..offset);
        Ok(events)
    }

    /// Check that a stream ended on a complete frame boundary.
    /// # Errors
    /// Returns `Truncated` for incomplete input or `Poisoned` after a framing failure.
    pub fn finish(&self) -> Result<(), CodecError> {
        if self.poisoned.get() {
            return Err(CodecError::Poisoned);
        }
        if self.buffer.is_empty() {
            Ok(())
        } else {
            self.poisoned.set(true);
            Err(CodecError::Truncated)
        }
    }
}

#[cfg(test)]
mod tests;
