//! Private World result used by the loopback scenario harness and its W1 child.
use super::{Controller, State};
use crate::Error;
use echo_core_api::{Broken, LinkState};
use rustix::fs::{self, Dir, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{fs::File, net::SocketAddr, os::fd::OwnedFd, path::Path, sync::Arc};

pub const DIRECTORY_ENV: &str = "ECHO_LAB_WORLD_CHECK_DIR";
pub const SEED_ENV: &str = "ECHO_LAB_WORLD_SEED";
pub const TRACE_TICK_ENV: &str = "ECHO_LAB_TRACE_TICK";
pub const REPORT_FILE: &str = "world-check.json";

pub(super) struct Witness {
    directory: OwnedFd,
    pub(super) seed: Option<u64>,
}

impl Witness {
    pub(super) fn open(local: bool) -> Result<Option<Self>, Error> {
        let Some(path) = std::env::var_os(DIRECTORY_ENV).filter(|_| local) else {
            return Ok(None);
        };
        let path = Path::new(&path);
        if !path.is_absolute() {
            return Err(Error::Input);
        }
        let directory = fs::open(
            path,
            OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| Error::Input)?;
        let metadata = fs::fstat(&directory).map_err(|_| Error::Input)?;
        if metadata.st_mode & 0o777 != 0o700 || metadata.st_uid != rustix::process::geteuid().as_raw() {
            return Err(Error::Input);
        }
        for entry in Dir::read_from(&directory).map_err(|_| Error::Input)? {
            if !matches!(entry.map_err(|_| Error::Input)?.file_name(), name if name == c"." || name == c"..") {
                return Err(Error::Input);
            }
        }
        let seed = std::env::var_os(SEED_ENV)
            .map(|seed| seed.to_str().and_then(|seed| seed.parse().ok()).ok_or(Error::Input))
            .transpose()?;
        Ok(Some(Self { directory, seed }))
    }

    pub(super) fn publish(&self, report: &Report) -> Result<(), Error> {
        let temporary = ".world-check.tmp";
        let fd = fs::openat(
            &self.directory,
            temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| Error::Control)?;
        let result = serde_json::to_writer(File::from(fd), report).map_err(|_| Error::Control);
        let result = result.and_then(|()| {
            fs::linkat(
                &self.directory,
                temporary,
                &self.directory,
                REPORT_FILE,
                fs::AtFlags::empty(),
            )
            .map_err(|_| Error::Control)
        });
        let cleanup = fs::unlinkat(&self.directory, temporary, fs::AtFlags::empty()).map_err(|_| Error::Control);
        result.and(cleanup)
    }
}

/// W1 uses the same controller with the raw Wyoming client and its own trace format.
pub struct HelperWorld {
    controller: Arc<Controller>,
    address: SocketAddr,
}

impl HelperWorld {
    /// # Errors
    /// Rejects an invalid private witness directory for the local helper.
    pub fn new(address: SocketAddr) -> Result<Self, Error> {
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(Error::Input);
        }
        Ok(Self {
            controller: Controller::with_witness(Witness::open(true)?),
            address,
        })
    }

    /// # Errors
    /// Reports transport admission errors from the Wyoming client.
    pub async fn connect(&self) -> Result<echo_wyoming::WyomingClient, echo_wyoming::Error> {
        echo_wyoming::WyomingClient::connect_endpoint(
            self.address.into(),
            echo_wyoming::ClientOptions::default(),
            self.controller.clone(),
        )
        .await
    }

    pub fn trace_failed(&self) {
        self.controller.trace_failed();
    }

    /// # Errors
    /// Reports failure to atomically publish the final private result.
    pub fn publish(&self) -> Result<(), Error> {
        self.controller.close_trace();
        self.controller.publish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    status: Status,
    snapshot: Snapshot,
    #[serde(deserialize_with = "Option::deserialize")]
    diff: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pass,
    Fail,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    device: Device,
    trace: Trace,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Device {
    connected: Flag,
    registered: Flag,
    run_open: Flag,
    capture_open: Flag,
    mic_muted: bool,
    mute_latched: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    speaker_volume: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    speaker_muted: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Flag(bool);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Trace {
    pending_rows: usize,
    writer_failed: bool,
}

const CAPTURE: &str = "device.capture_open: expected false, actual true";
const VOLUME: &str = "device.speaker_volume: expected 0..=100, actual out of range";
const DEADLINE: &str = "trace.deadline: expected present exactly while rows are pending, actual inconsistent";
const RUN: &str = "device.run_open: expected false, actual true";
const LINK: &str = "device.connected: expected false, actual true";
const ROWS: &str = "trace.pending_rows: expected 0, actual nonzero";
const WRITER: &str = "trace.writer_failed: expected false, actual true";
const PACER_DEPTH: &str = "pacer.reply_depth: expected 0..=128, actual out of range";
const SESSION_CLOSING: &str = "session.idle_deadline: expected absent while closing, actual present";
const PACER_QUIESCENT: &str = "pacer.quiescent: expected true, actual false";

impl Report {
    pub(super) fn from_state(state: &State) -> Self {
        let device = state.world.device.current();
        let snapshot = Snapshot {
            device: Device {
                connected: Flag(device.link != LinkState::Disconnected),
                registered: Flag(device.link == LinkState::Registered),
                run_open: Flag(device.run_open),
                capture_open: Flag(device.capture_open),
                mic_muted: device.privacy.mic_muted,
                mute_latched: device.privacy.mute_latched,
                speaker_volume: device.speaker_volume,
                speaker_muted: device.speaker_muted,
            },
            trace: Trace {
                pending_rows: state.world.trace.pending_rows(),
                writer_failed: state.world.trace.writer_failed(),
            },
        };
        let failure = state
            .first_violation
            .as_ref()
            .map(render)
            .or_else(|| state.world.check().as_ref().err().map(render))
            .or_else(|| (!state.world.pacer.playback.quiescent()).then(|| PACER_QUIESCENT.to_owned()))
            .or_else(|| snapshot.terminal_failure().map(str::to_owned));
        Self {
            status: if failure.is_none() { Status::Pass } else { Status::Fail },
            snapshot,
            diff: failure,
        }
    }

    /// Returns a fixed safe diff only for a valid failed report.
    /// # Errors
    /// Inconsistent status, snapshot or unrecognized diagnostic.
    pub fn verdict(&self) -> Result<Option<&'static str>, Error> {
        match (&self.status, self.diff.as_deref()) {
            (Status::Pass, None) if self.snapshot.terminal_failure().is_none() => Ok(None),
            (Status::Fail, Some(diff)) => [
                CAPTURE,
                VOLUME,
                DEADLINE,
                RUN,
                LINK,
                ROWS,
                WRITER,
                PACER_DEPTH,
                PACER_QUIESCENT,
                SESSION_CLOSING,
            ]
            .into_iter()
            .find(|known| *known == diff)
            .map(Some)
            .ok_or(Error::Control),
            _ => Err(Error::Control),
        }
    }
}

#[cfg(test)]
#[test]
fn pacer_world_report_accepts_only_fixed_diagnostics() {
    let controller = Controller::new();
    let mut report = Report::from_state(&controller.state());
    assert_eq!(report.verdict().unwrap(), None);
    {
        let mut state = controller.state();
        state.world.step(
            &mut echo_core_api::Event::Pacer(echo_core_api::PacerEvent::RunOpened {
                lead_ms: 200,
                adaptive: false,
            }),
            &mut echo_core_api::Commands::default(),
        );
        assert_eq!(Report::from_state(&state).verdict().unwrap(), Some(PACER_QUIESCENT));
    }
    report.status = Status::Fail;
    for diff in [PACER_DEPTH, PACER_QUIESCENT] {
        report.diff = Some(diff.to_owned());
        assert_eq!(report.verdict().unwrap(), Some(diff));
    }
    report.diff = Some(format!("{PACER_DEPTH} 129"));
    assert!(matches!(report.verdict(), Err(Error::Control)));
}

fn render(broken: &Broken) -> String {
    format!(
        "{}: expected {}, actual {}",
        broken.field, broken.expected, broken.actual
    )
}

impl Snapshot {
    fn terminal_failure(&self) -> Option<&'static str> {
        [
            (self.device.capture_open.0, CAPTURE),
            (self.device.speaker_volume.is_some_and(|volume| volume > 100), VOLUME),
            (self.device.run_open.0, RUN),
            (self.device.connected.0 || self.device.registered.0, LINK),
            (self.trace.pending_rows != 0, ROWS),
            (self.trace.writer_failed, WRITER),
        ]
        .into_iter()
        .find_map(|(failed, diff)| failed.then_some(diff))
    }
}
