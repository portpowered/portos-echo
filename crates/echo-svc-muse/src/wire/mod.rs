//! Non-generated protobuf for the daemon service and Noise transport.

mod chunk;
mod daemon;
mod proto;

pub(super) use chunk::{Assembler, Packetizer};
pub(super) use daemon::{Body, Frame, Header, Request, StreamMux, decode_response, encode_request};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidWire;

type Result<T> = std::result::Result<T, InvalidWire>;

#[cfg(test)]
pub(super) use daemon::{Response, decode_request, encode_response};

#[cfg(test)]
mod tests;
