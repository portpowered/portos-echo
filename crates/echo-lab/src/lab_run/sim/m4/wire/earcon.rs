//! Independent numeric observer for the chosen original wake waveform.
//! Never classify a cut reply, arbitrary quiet PCM, or unknown data as a chime.
use echo_contracts::wyoming::Event;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub(super) struct Earcon {
    pub bytes: usize,
    pub first: Option<Duration>,
    active: bool,
    continued: bool,
    settled: bool,
}

impl Earcon {
    pub fn observe(&mut self, event: &Event, now: Duration) -> Result<bool, String> {
        match event.header.r#type.as_str() {
            "hassmic-continue" if self.active => self.continued = true,
            "hassmic-flush" if self.active => {
                self.active = false;
                self.settled = true;
            }
            "audio-stop" if self.active => {
                if !self.continued {
                    return Err("wire: earcon ended capture instead of cutting".into());
                }
                self.active = false;
                self.settled = true;
            }
            "audio-chunk" => {
                let recognized = matches(&event.payload, self.bytes / 2);
                if self.active && !recognized {
                    return Err("wire: mixed or corrupted earcon PCM".into());
                }
                if recognized && !self.settled {
                    self.first.get_or_insert(now);
                    self.active = true;
                    self.bytes += event.payload.len();
                    return Ok(true);
                }
            }
            _ => {}
        }
        Ok(false)
    }
}

pub(super) fn matches(bytes: &[u8], offset: usize) -> bool {
    if bytes.len() != 960 || offset + 480 > 3840 {
        return false;
    }
    bytes.as_chunks::<2>().0.iter().enumerate().all(|(index, bytes)| {
        let index = offset + index;
        let position = u32::try_from(index % 1920).unwrap_or(1920);
        let edge = position.min(1919 - position).min(240);
        let fade = (1.0 - (std::f64::consts::PI * f64::from(edge) / 240.0).cos()) / 2.0;
        let frequency = if index < 1920 { 750.0 } else { 1000.0 };
        let expected =
            (983.0 * fade * (std::f64::consts::TAU * frequency * f64::from(position) / 24000.0).sin()).round();
        (f64::from(i16::from_le_bytes(*bytes)) - expected).abs() <= 1.0
    })
}
