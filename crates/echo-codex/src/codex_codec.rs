//! Bounded JSONL framing. Limits exclude LF and include any CR before LF.
use super::{
    codex_protocol::{Message, decode},
    codex_rpc::RpcError,
};
use serde::Serialize;
use std::io::{self, Write};

/// Incremental decoder; it never buffers more than the configured line length.
pub struct Decoder {
    bytes: Vec<u8>,
    limit: usize,
    failed: bool,
}

impl Decoder {
    /// # Errors
    /// A zero byte limit is invalid.
    pub fn new(limit: usize) -> Result<Self, RpcError> {
        if limit == 0 {
            return Err(RpcError::InvalidLimits);
        }
        Ok(Self {
            bytes: Vec::new(),
            limit,
            failed: false,
        })
    }

    /// Feed one byte. A complete physical line produces one decoded message.
    /// # Errors
    /// Malformed JSON and overflow permanently close the decoder.
    pub fn push(&mut self, byte: u8) -> Result<Option<Message>, RpcError> {
        if self.failed {
            return Err(RpcError::Disconnected);
        }
        let result = if byte == b'\n' {
            let result = decode(&self.bytes).map(Some);
            self.bytes.clear();
            result
        } else if self.bytes.len() == self.limit {
            Err(RpcError::LineTooLong)
        } else {
            self.bytes.push(byte);
            Ok(None)
        };
        if result.is_err() {
            self.failed = true;
            self.bytes.clear();
        }
        result
    }

    /// EOF is a connection failure, including when a partial line remains.
    pub fn eof(&mut self) -> RpcError {
        self.failed = true;
        self.bytes.clear();
        RpcError::Disconnected
    }
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
    overflowed: bool,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.overflowed = true;
            return Err(io::Error::other("line limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serialize directly into a bounded buffer; append exactly one LF.
/// # Errors
/// Reject invalid limits, oversized output and serialization failures.
pub fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, RpcError> {
    if limit == 0 {
        return Err(RpcError::InvalidLimits);
    }
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        limit,
        overflowed: false,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.overflowed {
            RpcError::LineTooLong
        } else {
            RpcError::Malformed
        });
    }
    writer.bytes.push(b'\n');
    Ok(writer.bytes)
}

#[cfg(test)]
#[path = "codex_codec_tests.rs"]
mod tests;
