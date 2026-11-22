use std::collections::HashMap;

use super::{
    InvalidWire, Result,
    proto::{Reader, Writer},
};

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) verb: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<Header>,
    pub(crate) body: Vec<u8>,
    pub(crate) end_body: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) status: i32,
    pub(crate) headers: Vec<Header>,
    pub(crate) body: Vec<u8>,
    pub(crate) end_body: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Body {
    Request(Request),
    Response(Response),
    Chunk { data: Vec<u8>, end_body: bool },
    Reset { code: i32, reason: String },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) id: i64,
    pub(crate) body: Body,
}

fn signed64(value: u64) -> i64 {
    i64::from_le_bytes(value.to_le_bytes())
}

fn signed32(value: u64) -> i32 {
    let [a, b, c, d, ..] = value.to_le_bytes();
    i32::from_le_bytes([a, b, c, d])
}

fn unsigned(value: i64) -> u64 {
    u64::from_le_bytes(value.to_le_bytes())
}

fn decode_header(bytes: &[u8]) -> Result<Header> {
    let mut header = Header::default();
    let mut fields = Reader(bytes);
    while let Some((tag, value)) = fields.next()? {
        match tag {
            1 => header.key = value.text()?,
            2 => header.value = value.text()?,
            _ => {}
        }
    }
    Ok(header)
}

fn encode_headers(fields: &mut Writer, tag: u32, headers: &[Header]) {
    for header in headers {
        let mut record = Writer::default();
        record.bytes(1, header.key.as_bytes());
        record.bytes(2, header.value.as_bytes());
        fields.message(tag, &record.finish());
    }
}

fn decode_application_request(data: &[u8]) -> Result<Request> {
    let mut request = Request::default();
    let mut fields = Reader(data);
    while let Some((tag, value)) = fields.next()? {
        match tag {
            1 => request.verb = value.text()?,
            2 => request.path = value.text()?,
            3 => request.headers.push(decode_header(value.bytes()?)?),
            4 => request.body = value.bytes()?.to_vec(),
            5 => request.end_body = value.uint()? != 0,
            _ => {}
        }
    }
    Ok(request)
}

fn decode_application_response(data: &[u8]) -> Result<Response> {
    let mut response = Response::default();
    let mut fields = Reader(data);
    while let Some((tag, value)) = fields.next()? {
        match tag {
            1 => response.status = signed32(value.uint()?),
            2 => response.headers.push(decode_header(value.bytes()?)?),
            3 => response.body = value.bytes()?.to_vec(),
            4 => response.end_body = value.uint()? != 0,
            _ => {}
        }
    }
    Ok(response)
}

fn decode_chunk(data: &[u8]) -> Result<Body> {
    let mut bytes = Vec::new();
    let mut end_body = false;
    let mut fields = Reader(data);
    while let Some((tag, value)) = fields.next()? {
        match tag {
            1 => bytes = value.bytes()?.to_vec(),
            2 => end_body = value.uint()? != 0,
            _ => {}
        }
    }
    Ok(Body::Chunk { data: bytes, end_body })
}

fn decode_reset(data: &[u8]) -> Result<Body> {
    let mut code = 0;
    let mut reason = String::new();
    let mut fields = Reader(data);
    while let Some((tag, value)) = fields.next()? {
        match tag {
            1 => code = signed32(value.uint()?),
            2 => reason = value.text()?,
            _ => {}
        }
    }
    Ok(Body::Reset { code, reason })
}

fn encode_frame(frame: &Frame) -> Vec<u8> {
    let mut body = Writer::default();
    let tag = match &frame.body {
        Body::Request(request) => {
            body.bytes(1, request.verb.as_bytes());
            body.bytes(2, request.path.as_bytes());
            encode_headers(&mut body, 3, &request.headers);
            body.bytes(4, &request.body);
            body.uint(5, u64::from(request.end_body));
            2
        }
        Body::Response(response) => {
            body.uint(1, unsigned(i64::from(response.status)));
            encode_headers(&mut body, 2, &response.headers);
            body.bytes(3, &response.body);
            body.uint(4, u64::from(response.end_body));
            3
        }
        Body::Chunk { data, end_body } => {
            body.bytes(1, data);
            body.uint(2, u64::from(*end_body));
            4
        }
        Body::Reset { code, reason } => {
            body.uint(1, unsigned(i64::from(*code)));
            body.bytes(2, reason.as_bytes());
            5
        }
    };
    let mut fields = Writer::default();
    fields.uint(1, unsigned(frame.id));
    fields.message(tag, &body.finish());
    fields.finish()
}

fn decode_frame(bytes: &[u8]) -> Result<Frame> {
    let mut id = 0;
    let mut body = None;
    let mut fields = Reader(bytes);
    while let Some((tag, value)) = fields.next()? {
        body = match tag {
            1 => {
                id = signed64(value.uint()?);
                body
            }
            2 => Some(Body::Request(decode_application_request(value.bytes()?)?)),
            3 => Some(Body::Response(decode_application_response(value.bytes()?)?)),
            4 => Some(decode_chunk(value.bytes()?)?),
            5 => Some(decode_reset(value.bytes()?)?),
            _ => body,
        };
    }
    Ok(Frame {
        id,
        body: body.ok_or(InvalidWire)?,
    })
}

pub(crate) fn encode_request(frame: &Frame) -> Vec<u8> {
    let mut envelope = Writer::default();
    // Service 0 (daemon) is the protobuf default.
    envelope.message(2, &encode_frame(frame));
    envelope.finish()
}

pub(crate) fn decode_response(data: &[u8]) -> Result<Frame> {
    decode_envelope(data, 1)
}

fn decode_envelope(data: &[u8], payload_tag: u32) -> Result<Frame> {
    let mut fields = Reader(data);
    let mut payload = &[][..];
    while let Some((tag, value)) = fields.next()? {
        if tag == payload_tag {
            payload = value.bytes()?;
        } else if tag == 1 && value.uint()? != 0 {
            return Err(InvalidWire);
        }
    }
    decode_frame(payload)
}

#[cfg(test)]
pub(crate) fn decode_request(data: &[u8]) -> Result<Frame> {
    decode_envelope(data, 2)
}

#[cfg(test)]
pub(crate) fn encode_response(frame: &Frame) -> Vec<u8> {
    let mut envelope = Writer::default();
    envelope.message(1, &encode_frame(frame));
    envelope.finish()
}

#[derive(Default)]
pub(crate) struct StreamMux {
    last_id: i64,
    incoming: HashMap<i64, bool>,
}

impl StreamMux {
    pub(crate) fn forget(&mut self, id: i64) -> bool {
        self.incoming.remove(&id).is_some()
    }

    pub(crate) fn open(&mut self, request: Request) -> Result<Frame> {
        self.last_id = self.last_id.checked_add(1).ok_or(InvalidWire)?;
        self.incoming.insert(self.last_id, false);
        Ok(Frame {
            id: self.last_id,
            body: Body::Request(request),
        })
    }

    pub(crate) fn accept(&mut self, frame: Frame) -> Option<(i64, Result<Body>)> {
        let response_seen = self.incoming.get_mut(&frame.id)?;
        let (closed, valid) = match &frame.body {
            Body::Response(response) if !*response_seen => {
                *response_seen = true;
                (response.end_body, true)
            }
            Body::Chunk { end_body, .. } if *response_seen => (*end_body, true),
            Body::Reset { .. } => (true, true),
            _ => (true, false),
        };
        if closed {
            self.incoming.remove(&frame.id);
        }
        Some((frame.id, if valid { Ok(frame.body) } else { Err(InvalidWire) }))
    }
}
