//! One RTP sequence space per negotiated peer, across all of its replies.
//! Tone payload indices restart each reply; transport identities never do.
#[derive(Default)]
pub(super) struct Stream {
    sequence: u16,
    timestamp: u32,
}

impl Stream {
    pub fn packet(&mut self, index: u16, quiet_edges: bool) -> Result<echo_live::AudioFrame, String> {
        let mut frame = super::super::tone::packet(index).ok_or("m2: invalid tone sequence")?;
        if quiet_edges && !(2..8).contains(&index) {
            // Standard 20 ms mono Opus digital silence, never microphone PCM.
            frame.payload = vec![0xf8, 0xff, 0xfe];
        }
        frame.sequence = self.sequence;
        frame.timestamp = self.timestamp;
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(960);
        Ok(frame)
    }
}

#[cfg(test)]
mod tests;
