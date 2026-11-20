//! Bounded data-only pack installation. No archive-controlled code is executed.
mod archive;
mod catalog;
mod css;
mod download;
pub(crate) mod http;
mod manifest;
#[cfg(test)]
mod tests;
pub(crate) mod view;
mod worker;

pub(crate) use worker::jobs;
pub(crate) use worker::{Downloads, start};

#[derive(Default)]
pub(crate) struct Files {
    pub root: std::sync::OnceLock<std::path::PathBuf>,
    pub busy: std::sync::Arc<tokio::sync::Mutex<()>>,
    pub jobs: tokio::sync::Mutex<jobs::Jobs>,
    pub requests: std::sync::OnceLock<tokio::sync::mpsc::Sender<jobs::Request>>,
    pub settings: std::sync::OnceLock<tokio::sync::mpsc::Sender<crate::SettingsRequest>>,
}

const MIB: u64 = 1024 * 1024;
const EXPANDED_LIMIT: u64 = 64 * MIB;
const TRANSFER_LIMIT: u64 = 65 * MIB;
const CLIP_LIMIT: u64 = 8 * MIB;
const FILE_LIMIT: usize = 64;

/// Diagnostics deliberately omit submitted URLs, paths and parser details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum Error {
    #[error("configured CA bundle unavailable")]
    Ca,
    #[error("expected HTTPS URL without credentials or fragment")]
    Url,
    #[error("download failed")]
    Download,
    #[error("download deadline exceeded")]
    Deadline,
    #[error("compressed transfer exceeds 65 MiB")]
    Transfer,
    #[error("sha256 mismatch or invalid digest")]
    Digest,
    #[error("invalid gzip or tar archive")]
    Archive,
    #[error("archive path must be normalized and relative")]
    Path,
    #[error("archive links are forbidden")]
    Link,
    #[error("archive special entries are forbidden")]
    Entry,
    #[error("duplicate archive path")]
    Duplicate,
    #[error("pack exceeds 64 files")]
    Count,
    #[error("expanded pack exceeds 64 MiB")]
    Size,
    #[error("clip exceeds 8 MiB")]
    Clip,
    #[error("unsupported file type")]
    Type,
    #[error("invalid pack.json")]
    Manifest,
    #[error("pack references a missing file or wrong file type")]
    Reference,
    #[error("unsafe or unsupported pack CSS")]
    Css,
    #[error("pack storage unavailable")]
    Storage,
    #[error("installed identity already exists with different contents")]
    Conflict,
    #[error("pack operation cancelled")]
    Cancelled,
}

fn relative(path: &str) -> Result<(), Error> {
    if path.is_empty()
        || !path.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(Error::Path);
    }
    Ok(())
}

fn extension(path: &str) -> Result<&str, Error> {
    let ext = path.rsplit_once('.').map(|(_, ext)| ext).ok_or(Error::Type)?;
    match ext {
        "json" | "css" | "png" | "webp" | "webm" | "mp4" => Ok(ext),
        _ => Err(Error::Type),
    }
}
