use super::{CLIP_LIMIT, EXPANDED_LIMIT, Error, FILE_LIMIT, MIB, TRANSFER_LIMIT, extension, manifest, relative};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

// Includes tar headers and padding, so metadata cannot turn a small payload into
// an unbounded decompression job. Raw entries reject extension metadata outright.
const TAR_LIMIT: u64 = EXPANDED_LIMIT + MIB;

pub(super) struct Prepared {
    stage: tempfile::TempDir,
    pub manifest: manifest::Manifest,
    digest: String,
}

pub(super) fn prepare(
    root: &Path,
    compressed: &Path,
    digest: &str,
    cancelled: &Arc<AtomicBool>,
) -> Result<Prepared, Error> {
    verify(compressed, digest, cancelled)?;
    let input = std::fs::File::open(compressed).map_err(|_| Error::Storage)?;
    let mut decoded = GzDecoder::new(input).take(TAR_LIMIT + 1);
    let files = contents(&mut decoded, cancelled)?;
    // Drain to verify gzip CRC/trailer and reject excessive padding/decompression.
    std::io::copy(&mut decoded, &mut std::io::sink()).map_err(|_| Error::Archive)?;
    if decoded.limit() == 0 {
        return Err(Error::Size);
    }
    let manifest = manifest::parse(&files)?;
    check_cancel(cancelled)?;
    let stage = tempfile::Builder::new()
        .prefix(".pack-stage-")
        .tempdir_in(root)
        .map_err(|_| Error::Storage)?;
    for (path, bytes) in files {
        check_cancel(cancelled)?;
        let destination = stage.path().join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|_| Error::Storage)?;
        }
        std::fs::write(destination, bytes).map_err(|_| Error::Storage)?;
    }
    Ok(Prepared {
        stage,
        manifest,
        digest: digest.to_ascii_lowercase(),
    })
}

fn verify(path: &Path, digest: &str, cancelled: &AtomicBool) -> Result<(), Error> {
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Digest);
    }
    let mut input = std::fs::File::open(path)
        .map_err(|_| Error::Storage)?
        .take(TRANSFER_LIMIT + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 16_384];
    loop {
        check_cancel(cancelled)?;
        let count = input.read(&mut buffer).map_err(|_| Error::Storage)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if input.limit() == 0 {
        return Err(Error::Transfer);
    }
    let actual = hash.finalize();
    if !actual
        .iter()
        .zip(digest.as_bytes().chunks_exact(2))
        .all(|(byte, pair)| {
            let value = |b: u8| (b as char).to_digit(16).unwrap_or(256);
            u32::from(*byte) == value(pair[0]) * 16 + value(pair[1])
        })
    {
        return Err(Error::Digest);
    }
    Ok(())
}

fn contents(reader: &mut impl Read, cancelled: &AtomicBool) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    let mut archive = tar::Archive::new(reader);
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for entry in archive.entries().map_err(|_| Error::Archive)?.raw(true) {
        check_cancel(cancelled)?;
        let mut entry = entry.map_err(|_| Error::Archive)?;
        if !entry.header().entry_type().is_file() {
            return Err(Error::Entry);
        }
        let path = std::str::from_utf8(&entry.path_bytes())
            .map_err(|_| Error::Path)?
            .to_owned();
        relative(&path)?;
        let ext = extension(&path)?;
        if files.contains_key(&path) {
            return Err(Error::Duplicate);
        }
        if files.len() == FILE_LIMIT {
            return Err(Error::Count);
        }
        let size = entry.size();
        if ["webm", "mp4"].contains(&ext) && size > CLIP_LIMIT {
            return Err(Error::Clip);
        }
        total = total.checked_add(size).ok_or(Error::Size)?;
        if total > EXPANDED_LIMIT {
            return Err(Error::Size);
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|_| Error::Archive)?;
        if bytes.len() as u64 != size {
            return Err(Error::Archive);
        }
        files.insert(path, bytes);
    }
    Ok(files)
}

impl Prepared {
    pub fn identity(&self) -> String {
        format!("{}@{}", self.manifest.name, self.manifest.version)
    }

    /// Called only by the lifetime/auth publication owner, after blocking work.
    pub fn publish(self, root: &Path, cancelled: &AtomicBool) -> Result<String, Error> {
        check_cancel(cancelled)?;
        let id = self.identity();
        let destination = root.join(&id);
        let marker = root.join(format!(".{id}.sha256"));
        if destination.exists() {
            return if std::fs::read_to_string(marker).ok().as_deref() == Some(&self.digest) {
                Ok(id)
            } else {
                Err(Error::Conflict)
            };
        }
        // The single owner serializes publication. No partial extracted tree is
        // ever an installed identity. A failed marker write cannot replace one.
        let mut pin = tempfile::NamedTempFile::new_in(root).map_err(|_| Error::Storage)?;
        std::io::Write::write_all(&mut pin, self.digest.as_bytes()).map_err(|_| Error::Storage)?;
        pin.persist(marker).map_err(|_| Error::Storage)?;
        std::fs::rename(self.stage.path(), destination).map_err(|_| Error::Storage)?;
        Ok(id)
    }
}

fn check_cancel(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
