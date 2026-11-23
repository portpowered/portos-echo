use super::{relay_request, screen_socket, wire};
use std::{
    ffi::OsString,
    io::{self, BufRead, Read, Write},
};
use wire::{Operation, Request};

/// Run the A2 USB entry point, keeping the private request on stdin.
#[must_use]
pub fn usb_command(args: impl IntoIterator<Item = OsString>) -> u8 {
    let mut args = args.into_iter();
    let operation = match (args.next().as_deref().and_then(std::ffi::OsStr::to_str), args.next()) {
        (Some("status"), None) => Operation::Status,
        (Some("join"), None) => Operation::Join,
        _ => return 2,
    };
    let Some(request) = input(operation) else {
        return 2;
    };
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()
        .and_then(|runtime| {
            let socket = screen_socket(true);
            runtime.block_on(relay_request(socket.as_deref(), &request))
        })
        .unwrap_or_else(|| wire::failure(Some(operation), wire::Error::Unavailable));
    let ok = serde_json::from_slice::<wire::ResultFrame>(&result).is_ok_and(|frame| frame.data.ok);
    let mut output = io::stdout().lock();
    if output
        .write_all(&result)
        .and_then(|()| output.write_all(b"\n"))
        .is_err()
    {
        return 1;
    }
    u8::from(!ok)
}

fn input(operation: Operation) -> Option<Vec<u8>> {
    if operation == Operation::Status {
        return Some(wire::encode_request(&Request::Status));
    }
    let mut frame = Vec::new();
    io::stdin().lock().take(1025).read_until(b'\n', &mut frame).ok()?;
    if frame.pop() != Some(b'\n') || wire::request(&frame).ok()?.operation() != operation {
        return None;
    }
    Some(frame)
}
