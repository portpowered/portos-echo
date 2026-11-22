use std::{collections::HashMap, time::Duration};

use tokio::time::Instant;

use super::{
    InvalidWire, Result,
    proto::{Reader, Writer},
};

const MAX_PAYLOAD: usize = 65_489;
const MAX_CHUNKS: usize = 256;
const MAX_ASSEMBLIES: usize = 16;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct Packetizer {
    next_id: u64,
}

impl Packetizer {
    pub(crate) fn split(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        let count = bytes.len().div_ceil(MAX_PAYLOAD).max(1);
        if count > MAX_CHUNKS || bytes.len() > MAX_BYTES {
            return Err(InvalidWire);
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .filter(|id| i64::try_from(*id).is_ok())
            .ok_or(InvalidWire)?;
        let chunks = bytes.chunks(MAX_PAYLOAD).chain(bytes.is_empty().then_some(&[][..]));
        Ok(chunks
            .enumerate()
            .map(|(index, payload)| {
                let mut frame = Writer::default();
                frame.uint(1, self.next_id);
                frame.uint(2, index as u64);
                frame.uint(3, count as u64);
                frame.bytes(4, payload);
                frame.finish()
            })
            .collect())
    }
}

struct Chunk<'a> {
    id: u64,
    index: usize,
    total: usize,
    payload: &'a [u8],
}

fn decode(data: &[u8]) -> Result<Chunk<'_>> {
    let mut chunk = Chunk {
        id: 0,
        index: 0,
        total: 1,
        payload: &[],
    };
    let mut fields = Reader(data);
    while let Some((tag, value)) = fields.next()? {
        match tag {
            1 => chunk.id = value.uint()?,
            2 => chunk.index = usize::try_from(value.uint()?).map_err(|_| InvalidWire)?,
            3 => chunk.total = usize::try_from(value.uint()?).map_err(|_| InvalidWire)?,
            4 => chunk.payload = value.bytes()?,
            _ => {}
        }
    }
    if chunk.total > MAX_CHUNKS || chunk.index >= chunk.total || chunk.payload.len() > MAX_PAYLOAD {
        return Err(InvalidWire);
    }
    Ok(chunk)
}

struct Assembly {
    created: Instant,
    chunks: Vec<Option<Vec<u8>>>,
    bytes: usize,
    received: usize,
}

#[derive(Default)]
pub(crate) struct Assembler {
    pending: HashMap<u64, Assembly>,
}

impl Assembler {
    pub(crate) fn push(&mut self, frame: &[u8], now: Instant) -> Result<Option<Vec<u8>>> {
        self.pending.retain(|_, parts| now.duration_since(parts.created) <= TTL);
        let chunk = decode(frame)?;
        if chunk.total == 1 {
            return if self.pending.contains_key(&chunk.id) {
                Err(InvalidWire)
            } else {
                Ok(Some(chunk.payload.to_vec()))
            };
        }
        let bytes = self.pending.values().map(|assembly| assembly.bytes).sum::<usize>();
        if bytes + chunk.payload.len() > MAX_BYTES
            || (!self.pending.contains_key(&chunk.id) && self.pending.len() >= MAX_ASSEMBLIES)
        {
            return Err(InvalidWire);
        }
        let assembly = self.pending.entry(chunk.id).or_insert_with(|| Assembly {
            created: now,
            chunks: vec![None; chunk.total],
            bytes: 0,
            received: 0,
        });
        if assembly.chunks.len() != chunk.total || assembly.chunks[chunk.index].is_some() {
            return Err(InvalidWire);
        }
        assembly.chunks[chunk.index] = Some(chunk.payload.to_vec());
        assembly.bytes += chunk.payload.len();
        assembly.received += 1;
        if assembly.received != chunk.total {
            return Ok(None);
        }
        let assembly = self.pending.remove(&chunk.id).ok_or(InvalidWire)?;
        let mut merged = Vec::with_capacity(assembly.bytes);
        for part in assembly.chunks.into_iter().flatten() {
            merged.extend(part);
        }
        Ok(Some(merged))
    }
}
