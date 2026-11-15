use crate::Error;
use std::time::Duration;

/// Queue limits and operation deadlines. Frame limits include the wire header.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub mic_capacity: usize,
    pub reply_capacity: usize,
    pub event_capacity: usize,
    pub command_capacity: usize,
    pub max_frame_bytes: usize,
    pub connect_timeout: Duration,
    pub operation_timeout: Duration,
    pub reconnect_initial: Duration,
    pub reconnect_cap: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            mic_capacity: 32,
            reply_capacity: 32,
            event_capacity: 32,
            command_capacity: 16,
            max_frame_bytes: 1_048_576,
            connect_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(5),
            reconnect_initial: Duration::from_millis(100),
            reconnect_cap: Duration::from_millis(2000),
        }
    }
}

impl ClientOptions {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let capacities = [
            self.mic_capacity,
            self.reply_capacity,
            self.event_capacity,
            self.command_capacity,
            self.max_frame_bytes,
        ];
        let deadlines = [
            self.connect_timeout,
            self.operation_timeout,
            self.reconnect_initial,
            self.reconnect_cap,
        ];
        if capacities.contains(&0)
            || deadlines.iter().any(Duration::is_zero)
            || self.reconnect_initial > self.reconnect_cap
        {
            return Err(Error::InvalidOptions);
        }
        Ok(())
    }
}
