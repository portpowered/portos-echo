use crate::{Shutter, switch};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    os::{
        fd::{AsFd, AsRawFd},
        unix::fs::{FileTypeExt as _, OpenOptionsExt as _},
    },
    path::Path,
};
use tokio::io::{Interest, unix::AsyncFd};

const BITMAP_BYTES: usize = 8;
const NAME_BYTES: usize = 128;
const EVENT_BYTES: usize = std::mem::size_of::<libc::timeval>() + 8;

/// One request owns an event queue and an independent fresh bitmap reader.
/// On Linux 4.4 EVIOCGSW flushes queued switch events on its own descriptor.
pub struct Observation {
    events: File,
    current: File,
}

impl Observation {
    /// Register a separately owned descriptor for the same evdev event queue.
    ///
    /// # Panics
    /// Requires a Tokio runtime with its I/O driver enabled.
    #[must_use]
    #[expect(unsafe_code, reason = "Stable descriptor ownership for Android evdev readiness")]
    pub fn readiness(&self) -> Option<AsyncFd<File>> {
        let events = self.events.try_clone().ok()?;
        // SAFETY: File owns the duplicated nonblocking descriptor until AsyncFd
        // drops it. Each readiness check refers to that same open description.
        unsafe { AsyncFd::register_with_interest(events, Interest::READABLE) }.ok()
    }

    /// Select the named camera switch without relying on a stable event number.
    #[must_use]
    pub fn open() -> Option<Self> {
        let mut matches = std::fs::read_dir("/dev/input").ok()?.filter_map(|entry| {
            let path = entry.ok()?.path();
            let number = path.file_name()?.to_str()?.strip_prefix("event")?;
            if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let events = open(&path)?;
            eligible(&events).then_some((events, path))
        });
        let (events, path) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        let current = open(&path)?;
        eligible(&current).then_some(Self { events, current })
    }

    /// Check queued transitions before and after the fresh switch bitmap.
    /// The caller stops using this observation on its first non-open reading.
    #[must_use]
    pub fn check(&self) -> Shutter {
        let queued = drain(&self.events);
        if queued != Shutter::Open {
            return queued;
        }
        let reading = query::<BITMAP_BYTES>(&self.current, 0x1b)
            .map_or(Shutter::Unknown, |(data, len)| switch::switch(&data[..len]));
        if reading != Shutter::Open {
            return reading;
        }
        drain(&self.events)
    }

    /// Drain queued events for asynchronous observation, without a state query.
    #[must_use]
    pub fn transitions(&self) -> Shutter {
        drain(&self.events)
    }
}

fn open(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    file.metadata().ok()?.file_type().is_char_device().then_some(file)
}

fn eligible(file: &File) -> bool {
    let Some((name, name_len)) = query::<NAME_BYTES>(file, 0x06) else {
        return false;
    };
    query::<BITMAP_BYTES>(file, 0x20 + switch::EV_SW)
        .is_some_and(|(bits, bits_len)| switch::supported(&name[..name_len], &bits[..bits_len]))
}

#[expect(
    unsafe_code,
    reason = "Android evdev read-only ioctl boundary documented in the camera ADR"
)]
fn query<const N: usize>(file: &File, operation: u8) -> Option<([u8; N], usize)> {
    let size = u32::try_from(N).ok()?;
    let request = (2 << 30) | (size << 16) | (u32::from(b'E') << 8) | u32::from(operation);
    let operation = libc::Ioctl::from_ne_bytes(request.to_ne_bytes());
    let mut buffer = [0_u8; N];
    // SAFETY: the borrowed file remains open. These Android ARM evdev ioctl
    // numbers encode a kernel write of at most N bytes. The initialized array
    // is valid for that write and stays live for the call.
    let copied = unsafe { libc::ioctl(file.as_fd().as_raw_fd(), operation, buffer.as_mut_ptr()) };
    let copied = usize::try_from(copied).ok()?;
    (copied <= N).then_some((buffer, copied))
}

fn drain(mut file: &File) -> Shutter {
    let mut data = [0_u8; EVENT_BYTES];
    for _ in 0..256 {
        match file.read(&mut data) {
            Ok(EVENT_BYTES) => {
                let offset = EVENT_BYTES - 8;
                let kind = u16::from_ne_bytes([data[offset], data[offset + 1]]);
                let code = u16::from_ne_bytes([data[offset + 2], data[offset + 3]]);
                let value =
                    i32::from_ne_bytes([data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]]);
                let state = switch::event(kind, code, value);
                if state != Shutter::Open {
                    return state;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Shutter::Open,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            _ => return Shutter::Unknown,
        }
    }
    Shutter::Unknown
}
