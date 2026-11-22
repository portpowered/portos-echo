//! Borrowed reads and writes through the pinned Wyoming framing helpers.

#![expect(unsafe_code, reason = "Wyoming reader and writer C ABI")]

use std::{
    ffi::{CStr, c_char, c_int, c_long, c_void},
    ptr, slice,
};

const MAX_JSON: usize = 8192;
pub(super) const MAX_PAYLOAD: usize = 1 << 20;

#[repr(C)]
struct NativeReader {
    fd: c_int,
    buf: *mut u8,
}

#[repr(C)]
struct NativeEvent {
    kind: [c_char; 48],
    json: [c_char; MAX_JSON],
    payload: *const u8,
    payload_len: usize,
}

unsafe extern "C" {
    fn wy_reader_init(reader: *mut NativeReader, fd: c_int) -> c_int;
    fn wy_reader_free(reader: *mut NativeReader);
    fn wy_read(reader: *mut NativeReader, event: *mut NativeEvent) -> c_int;
    fn wy_write(fd: c_int, kind: *const c_char, data: *const c_char, payload: *const c_void, len: usize) -> c_int;
    fn wy_json_int(json: *const c_char, key: *const c_char, out: *mut c_long) -> c_int;
    fn wy_json_str(json: *const c_char, key: *const c_char, out: *mut c_char, size: usize) -> c_int;
    fn wy_failsafe_seconds(json: *const c_char) -> c_int;
    fn wy_speaker_volume(json: *const c_char) -> c_int;
    fn wy_output_mute(json: *const c_char) -> c_int;
}

/// A framed event. The payload is valid until the reader advances.
pub struct Event<'a> {
    pub kind: &'a CStr,
    pub json: &'a CStr,
    pub payload: &'a [u8],
}

impl Event<'_> {
    /// Reads an integer using the pinned flat lookup and native unsigned conversion.
    #[must_use]
    pub fn audio_parameter(&self, key: &CStr, default: u32) -> u32 {
        let Some(value) = self.integer(key) else {
            return default;
        };
        // C converts the parsed long to unsigned when calling core_tts_begin.
        let bytes = value.to_le_bytes();
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    #[must_use]
    pub fn integer(&self, key: &CStr) -> Option<c_long> {
        let mut value = 0;
        // SAFETY: Both inputs are terminated strings and the native long is writable.
        let found = unsafe { wy_json_int(self.json.as_ptr(), key.as_ptr(), &raw mut value) };
        (found != 0).then_some(value)
    }

    /// Reads the pinned flat string field, with the original transcript data placement.
    #[must_use]
    pub fn text<'a>(&self, key: &CStr, from_data: bool, buffer: &'a mut [u8]) -> &'a CStr {
        let json = if from_data {
            let bytes = self.json.to_bytes_with_nul();
            bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .and_then(|newline| CStr::from_bytes_with_nul(&bytes[newline + 1..]).ok())
                .unwrap_or(self.json)
        } else {
            self.json
        };
        buffer.fill(0);
        if !buffer.is_empty() {
            // SAFETY: The strings are terminated. The extractor writes at most
            // buffer.len() bytes including the terminator.
            unsafe { wy_json_str(json.as_ptr(), key.as_ptr(), buffer.as_mut_ptr().cast(), buffer.len()) };
        }
        CStr::from_bytes_until_nul(buffer).unwrap_or(c"")
    }

    #[must_use]
    pub fn failsafe_seconds(&self) -> Option<i32> {
        // SAFETY: The extractor reads the terminated string during this call.
        let seconds = unsafe { wy_failsafe_seconds(self.json.as_ptr()) };
        (seconds > 0).then_some(seconds)
    }

    #[must_use]
    pub fn speaker_volume(&self) -> Option<i32> {
        // SAFETY: The extractor reads the terminated string during this call.
        let volume = unsafe { wy_speaker_volume(self.json.as_ptr()) };
        (volume >= 0).then_some(volume)
    }

    #[must_use]
    pub fn output_mute(&self) -> Option<bool> {
        // SAFETY: The extractor reads the terminated string during this call.
        match unsafe { wy_output_mute(self.json.as_ptr()) } {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

pub(super) struct Reader {
    reader: NativeReader,
    event: NativeEvent,
}

impl Reader {
    pub(super) fn new(fd: c_int) -> Option<Self> {
        let mut reader = NativeReader {
            fd,
            buf: ptr::null_mut(),
        };
        // SAFETY: The reader is writable and owns the allocation made on success.
        if unsafe { wy_reader_init(&raw mut reader, fd) } < 0 {
            return None;
        }
        Some(Self {
            reader,
            event: NativeEvent {
                kind: [0; 48],
                json: [0; MAX_JSON],
                payload: ptr::null(),
                payload_len: 0,
            },
        })
    }

    pub(super) fn read(&mut self) -> Option<Event<'_>> {
        // SAFETY: Both structures are initialized and the C reader owns its live buffer.
        if unsafe { wy_read(&raw mut self.reader, &raw mut self.event) } <= 0 {
            return None;
        }
        // SAFETY: A successful read terminates both arrays and bounds payload_len
        // to the reader allocation. All three borrows end before the next read.
        Some(unsafe {
            Event {
                kind: CStr::from_ptr(self.event.kind.as_ptr()),
                json: CStr::from_ptr(self.event.json.as_ptr()),
                payload: slice::from_raw_parts(self.event.payload, self.event.payload_len),
            }
        })
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: This reader exclusively owns the allocation made by wy_reader_init.
        unsafe { wy_reader_free(&raw mut self.reader) };
    }
}

pub(super) fn write(fd: c_int, kind: &CStr, data: Option<&CStr>, payload: &[u8]) -> bool {
    let data = data.map_or(ptr::null(), CStr::as_ptr);
    // SAFETY: The arguments are valid for the call. The caller serializes writes
    // and owns publication of the borrowed connection until C closes it.
    unsafe { wy_write(fd, kind.as_ptr(), data, payload.as_ptr().cast(), payload.len()) >= 0 }
}

#[cfg(test)]
mod tests {
    use super::write;
    use std::{
        fs::{self, OpenOptions},
        io::{Read, Seek},
        os::fd::AsRawFd,
    };

    #[test]
    fn pinned_writer_preserves_header_order_and_omits_empty_segments() {
        let path = std::env::temp_dir().join(format!("echo-device-wire-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        fs::remove_file(path).unwrap();
        assert!(write(
            file.as_raw_fd(),
            c"hassmic-speaker",
            Some(c"{\"volume\":40,\"muted\":false}"),
            &[],
        ));
        assert!(write(file.as_raw_fd(), c"audio-chunk", Some(c"{}"), b"\x13\x00"));
        assert!(write(file.as_raw_fd(), c"pong", None, &[]));
        file.rewind().unwrap();
        let mut observed = Vec::new();
        file.read_to_end(&mut observed).unwrap();
        assert_eq!(
            observed,
            b"{\"type\":\"hassmic-speaker\",\"version\":\"1.5.4\",\"data_length\":27}\n{\"volume\":40,\"muted\":false}\
              {\"type\":\"audio-chunk\",\"version\":\"1.5.4\",\"data_length\":2,\"payload_length\":2}\n{}\x13\x00\
              {\"type\":\"pong\",\"version\":\"1.5.4\"}\n"
        );
    }
}
