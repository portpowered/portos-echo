use super::state::ServiceClock;
use serde::Deserialize;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Shutdown,
    SetTime { unix_seconds: u64 },
}

pub(super) async fn control(clock: ServiceClock) -> Result<(), &'static str> {
    let mut input = BufReader::new(tokio::io::stdin());
    loop {
        let mut line = Vec::new();
        let count = (&mut input)
            .take(1025)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| "control_io_failed")?;
        if count == 0 {
            return Ok(());
        }
        if count > 1024 || line.last() != Some(&b'\n') {
            return Err("invalid_control");
        }
        match serde_json::from_slice::<Control>(&line).map_err(|_| "invalid_control")? {
            Control::Shutdown => return Ok(()),
            Control::SetTime { unix_seconds } => {
                clock.set(unix_seconds)?;
                writeln!(std::io::stdout(), "{{\"clock_unix_seconds\":{unix_seconds}}}")
                    .map_err(|_| "control_io_failed")?;
            }
        }
    }
}
