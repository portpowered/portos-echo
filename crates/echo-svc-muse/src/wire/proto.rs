use super::{InvalidWire, Result};

#[derive(Default)]
pub(super) struct Writer(Vec<u8>);

impl Writer {
    pub(super) fn uint(&mut self, field: u32, value: u64) {
        if value != 0 {
            self.varint(u64::from(field) << 3);
            self.varint(value);
        }
    }

    pub(super) fn bytes(&mut self, field: u32, data: &[u8]) {
        if !data.is_empty() {
            self.message(field, data);
        }
    }

    pub(super) fn message(&mut self, field: u32, data: &[u8]) {
        self.varint((u64::from(field) << 3) | 2);
        self.varint(data.len() as u64);
        self.0.extend_from_slice(data);
    }

    pub(super) fn finish(self) -> Vec<u8> {
        self.0
    }

    fn varint(&mut self, mut value: u64) {
        while value >= 128 {
            self.0.push(value.to_le_bytes()[0] | 128);
            value >>= 7;
        }
        self.0.push(value.to_le_bytes()[0]);
    }
}

pub(super) struct Reader<'a>(pub(super) &'a [u8]);

impl<'a> Reader<'a> {
    pub(super) fn next(&mut self) -> Result<Option<(u32, Field<'a>)>> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let key = u32::try_from(self.varint()?).map_err(|_| InvalidWire)?;
        let field = key >> 3;
        if field == 0 {
            return Err(InvalidWire);
        }
        let value = match key & 7 {
            0 => Field::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Field::Fixed
            }
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| InvalidWire)?;
                Field::Bytes(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Field::Fixed
            }
            _ => return Err(InvalidWire),
        };
        Ok(Some((field, value)))
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let (&part, rest) = self.0.split_first().ok_or(InvalidWire)?;
            self.0 = rest;
            if shift == 63 && part > 1 {
                return Err(InvalidWire);
            }
            value |= u64::from(part & 127) << shift;
            if part < 128 {
                return Ok(value);
            }
        }
        Err(InvalidWire)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let (field, rest) = self.0.split_at_checked(len).ok_or(InvalidWire)?;
        self.0 = rest;
        Ok(field)
    }
}

pub(super) enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

impl<'a> Field<'a> {
    pub(super) fn uint(&self) -> Result<u64> {
        match self {
            Self::Varint(value) => Ok(*value),
            _ => Err(InvalidWire),
        }
    }

    pub(super) fn bytes(&self) -> Result<&'a [u8]> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            _ => Err(InvalidWire),
        }
    }

    pub(super) fn text(&self) -> Result<String> {
        std::str::from_utf8(self.bytes()?)
            .map(str::to_owned)
            .map_err(|_| InvalidWire)
    }
}
