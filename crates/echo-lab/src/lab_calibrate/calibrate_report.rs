use super::calibrate_decode::Point;
use super::calibrate_fit::{self, AxisFit};
use crate::lab_probe::probe_parse::probe_inputs::TouchDevice;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct Target {
    pub name: &'static str,
    pub x: u32,
    pub y: u32,
}

pub(super) const TARGETS: [Target; 5] = [
    Target {
        name: "top-left",
        x: 64,
        y: 64,
    },
    Target {
        name: "top-right",
        x: 1216,
        y: 64,
    },
    Target {
        name: "bottom-right",
        x: 1216,
        y: 736,
    },
    Target {
        name: "bottom-left",
        x: 64,
        y: 736,
    },
    Target {
        name: "centre",
        x: 640,
        y: 400,
    },
];

#[derive(Serialize)]
pub(super) struct Tap {
    pub device: String,
    pub raw_x: i64,
    pub raw_y: i64,
    pub target: Target,
}

#[derive(Serialize)]
pub(super) struct Fit {
    pub x: AxisFit,
    pub y: AxisFit,
    pub max_residual_px: f64,
}

#[derive(Serialize)]
pub(super) struct Report {
    pub firmware_build: String,
    pub devices: Vec<TouchDevice>,
    pub taps: Vec<Tap>,
    pub glass: String,
    pub fit: Option<Fit>,
}

impl Report {
    pub fn add(&mut self, point: Point) -> Result<usize, String> {
        if !self.devices.iter().any(|d| d.node == point.node) {
            return Err("calibrate: tap from unknown device".into());
        }
        let count = self.taps.iter().filter(|tap| tap.device == point.node).count();
        let target = *TARGETS.get(count).ok_or("calibrate: too many taps")?;
        self.taps.push(Tap {
            device: point.node,
            raw_x: point.x,
            raw_y: point.y,
            target,
        });
        Ok(count + 1)
    }

    pub fn finish(&mut self) -> Result<(), String> {
        let Some(first) = self.taps.first() else {
            return Ok(());
        };
        if self.taps.iter().any(|tap| tap.device != first.device) {
            self.glass = "ambiguous".into();
            return Err("calibrate: taps came from more than one device".into());
        }
        if self.taps.len() != 5 {
            return Ok(());
        }
        self.glass = self
            .devices
            .iter()
            .find(|d| d.node == first.device)
            .ok_or("calibrate: tap from unknown device")?
            .name
            .clone();
        let x = fit_axis(&self.taps, |t| (t.raw_x, t.target.x))?;
        let y = fit_axis(&self.taps, |t| (t.raw_y, t.target.y))?;
        let max_residual_px = x.max_residual_px.max(y.max_residual_px);
        self.fit = Some(Fit { x, y, max_residual_px });
        calibrate_fit::acceptable(max_residual_px)
    }

    pub fn publish(&self, directory: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| "calibrate: JSON encoding failed")?;
        let pending = directory.join("calibrate.pending");
        std::fs::write(&pending, bytes).map_err(|_| "calibrate: report write failed")?;
        std::fs::rename(pending, directory.join("calibrate.json"))
            .map_err(|_| "calibrate: report publish failed".into())
    }
}

fn fit_axis(taps: &[Tap], coordinates: impl Fn(&Tap) -> (i64, u32)) -> Result<AxisFit, String> {
    let points: Result<Vec<_>, &str> = taps
        .iter()
        .map(|tap| {
            let (raw, panel) = coordinates(tap);
            // Android ABS coordinates are signed 32-bit values; reject out-of-contract input.
            let raw = i32::try_from(raw).map_err(|_| "calibrate: coordinate out of range")?;
            Ok((f64::from(raw), f64::from(panel)))
        })
        .collect();
    calibrate_fit::axis(&points?)
}
