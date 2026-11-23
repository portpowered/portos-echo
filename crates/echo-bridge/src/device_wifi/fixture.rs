//! Linux radio substitute for prebuilt-device and echo-lab journeys.
use super::wire::{self, Error, Reading, Request, ScanEntry};
use echo_device::wifi::{self, Join, Status};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    status: Option<Reading>,
    scan: Option<Reading>,
    #[serde(default)]
    association_gate: Option<PathBuf>,
}

pub(super) fn operate(_socket: &Path, request: Request) -> Result<Vec<u8>, Error> {
    let path = PathBuf::from(std::env::var_os("ECHO_LAB_WIFI_FIXTURE").ok_or(Error::Unsupported)?);
    let file = File::open(&path).map_err(|_| Error::Unavailable)?;
    let fixture = serde_json::from_reader(file.take(wire::RESULT_LIMIT)).map_err(|_| Error::Unavailable)?;
    let op = request.operation();
    let mut radio = Radio {
        path,
        fixture,
        candidate: None,
        previous: None,
    };
    let reading = match request {
        Request::Status => radio.fixture.status,
        Request::Scan => radio.fixture.scan,
        Request::Join { ssid, password } => {
            return Ok(wire::join_result(wifi::join(&mut radio, &ssid, password.expose())));
        }
        Request::Forget { network_id } => Some(radio.forget(network_id)?.into()),
    };
    Ok(wire::success(op, reading.ok_or(Error::Unavailable)?.project()))
}

struct Radio {
    path: PathBuf,
    fixture: Fixture,
    candidate: Option<(usize, bool)>,
    previous: Option<(Status, Option<i32>)>,
}

impl Radio {
    fn networks(&mut self) -> Option<&mut Vec<ScanEntry>> {
        match &mut self.fixture.scan {
            Some(Reading::Scan { networks }) => Some(networks),
            _ => None,
        }
    }

    fn save(&self) -> Option<()> {
        let staged = self.path.with_extension("wifi-next");
        fs::write(&staged, serde_json::to_vec(&self.fixture).ok()?).ok()?;
        fs::rename(staged, &self.path).ok()
    }

    fn forget(&mut self, id: i32) -> Result<Status, Error> {
        let status = self.status().ok_or(Error::Unavailable)?;
        let mut networks = self.networks().ok_or(Error::Unavailable)?.iter_mut();
        let row = networks.find(|net| net.network_id == Some(id)).ok_or(Error::NotSaved)?;
        let was_current = Some(&row.ssid) == status.ssid.as_ref();
        row.network_id = None;
        if was_current {
            self.fixture.status = Some(Reading::Status {
                ssid: None,
                reachable: false,
            });
        }
        self.save().ok_or(Error::Unavailable)?;
        self.status().ok_or(Error::Unavailable)
    }

    fn await_association(&self) -> Option<()> {
        if let Some(path) = &self.fixture.association_gate {
            let mut gate = UnixStream::connect(path).ok()?;
            gate.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
            gate.write_all(b"J").ok()?;
            gate.read_exact(&mut [0]).ok()?;
        }
        Some(())
    }
}

impl Join for Radio {
    fn prepare(&mut self, ssid: &str, password: &str) -> bool {
        let Some(status) = self.status() else { return false };
        let Some(networks) = self.networks() else { return false };
        let Some(index) = networks.iter().position(|network| network.ssid == ssid) else {
            return false;
        };
        let network = &networks[index];
        let valid = if network.secured {
            password == format!("join-{ssid}")
        } else {
            password.is_empty()
        };
        let saved = network.network_id;
        self.previous = Some((status, saved));
        self.candidate = Some((index, valid));
        true
    }

    fn associate(&mut self) -> Option<Status> {
        let (index, reachable) = self.candidate?;
        let networks = self.networks()?;
        let id = networks
            .iter()
            .filter_map(|row| row.network_id)
            .max()
            .unwrap_or(0)
            .checked_add(1)?;
        let row = &mut networks[index];
        row.network_id.get_or_insert(id);
        let ssid = Some(row.ssid.clone());
        self.fixture.status = Some(Reading::Status { ssid, reachable });
        self.save()?;
        self.await_association()?;
        self.status()
    }

    fn restore(&mut self) -> Option<Status> {
        let (index, _) = self.candidate?;
        let (status, saved) = self.previous.take()?;
        let networks = self.networks()?;
        networks[index].network_id = saved;
        self.fixture.status = Some(status.into());
        self.save()?;
        self.status()
    }

    fn status(&mut self) -> Option<Status> {
        match self.fixture.status.as_ref()? {
            Reading::Status { ssid, reachable } => Some(Status {
                ssid: ssid.clone(),
                reachable: *reachable,
            }),
            Reading::Scan { .. } => None,
        }
    }
}
