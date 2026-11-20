use super::{Prompt, Provider, prompt};
use crate::{Failure, Result};
use echo_screen_contract::ScreenFrame;
use std::collections::VecDeque;

const LIMIT: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct Events {
    line: Vec<u8>,
    data: String,
    sequence: Option<u64>,
    frames: VecDeque<ScreenFrame>,
}

impl Events {
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        for byte in bytes {
            if *byte == b'\n' {
                self.end_line()?;
                self.line.clear();
            } else {
                if self.line.len() + self.data.len() >= LIMIT {
                    return Err(Failure("setup_screen_limit"));
                }
                self.line.push(*byte);
            }
        }
        Ok(())
    }

    fn end_line(&mut self) -> Result<()> {
        let line = std::str::from_utf8(&self.line).map_err(|_| Failure("setup_screen_invalid"))?;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !self.data.is_empty() {
                let frame: ScreenFrame =
                    serde_json::from_str(&self.data).map_err(|_| Failure("setup_screen_invalid"))?;
                if self.sequence.is_some_and(|seq| frame.seq <= seq) {
                    return Err(Failure("setup_screen_order"));
                }
                self.sequence = Some(frame.seq);
                if self.frames.len() >= 32 {
                    return Err(Failure("setup_screen_limit"));
                }
                self.frames.push_back(frame);
                self.data.clear();
            }
        } else if let Some(data) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
        }
        Ok(())
    }

    pub fn next(&mut self, provider: Provider, now: u64) -> Result<Option<Prompt>> {
        while let Some(frame) = self.frames.pop_front() {
            if let Some(prompt) = prompt::project(frame, provider, now)? {
                return Ok(Some(prompt));
            }
        }
        Ok(None)
    }
}
