//! Revalidate editable local data; serving consumes these exact validated bytes.
use super::{CLIP_LIMIT, EXPANDED_LIMIT, Error, FILE_LIMIT, extension, manifest, relative};
use std::{collections::BTreeMap, fs, io::Read, path::Path};

pub(super) fn read(root: &Path, id: &str) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') {
        return Err(Error::Path);
    }
    directory(root)?;
    let root = root.canonicalize().map_err(|_| Error::Storage)?;
    let pack = root.join(id);
    directory(&pack)?;
    if !pack.canonicalize().map_err(|_| Error::Storage)?.starts_with(&root) {
        return Err(Error::Path);
    }
    let files = contents(&pack)?;
    let manifest = manifest::parse(&files)?;
    if id != manifest.name && id != format!("{}@{}", manifest.name, manifest.version) {
        return Err(Error::Manifest);
    }
    Ok(files)
}

fn directory(path: &Path) -> Result<(), Error> {
    let meta = fs::symlink_metadata(path).map_err(|_| Error::Storage)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::Path);
    }
    Ok(())
}

fn contents(pack: &Path) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    let mut pending = vec![pack.to_owned()];
    let mut files = BTreeMap::new();
    let mut remaining = EXPANDED_LIMIT;
    while let Some(dir) = pending.pop() {
        directory(&dir)?;
        for entry in fs::read_dir(dir).map_err(|_| Error::Storage)? {
            let path = entry.map_err(|_| Error::Storage)?.path();
            let name = path.strip_prefix(pack).map_err(|_| Error::Path)?;
            let name = name.to_str().ok_or(Error::Path)?;
            relative(name)?;
            let meta = fs::symlink_metadata(&path).map_err(|_| Error::Storage)?;
            if meta.file_type().is_symlink() {
                return Err(Error::Link);
            }
            if !path.canonicalize().map_err(|_| Error::Storage)?.starts_with(pack) {
                return Err(Error::Path);
            }
            if meta.is_dir() {
                pending.push(path);
                continue;
            }
            if files.len() == FILE_LIMIT {
                return Err(Error::Count);
            }
            let ext = extension(name)?;
            let cap = if ["webm", "mp4"].contains(&ext) {
                CLIP_LIMIT.min(remaining)
            } else {
                remaining
            };
            let bytes = regular(&path, cap)?;
            remaining = remaining.checked_sub(bytes.len() as u64).ok_or(Error::Size)?;
            files.insert(name.to_owned(), bytes);
        }
    }
    Ok(files)
}

fn regular(path: &Path, cap: u64) -> Result<Vec<u8>, Error> {
    let meta = fs::symlink_metadata(path).map_err(|_| Error::Storage)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::Entry);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(Error::Link);
        }
    }
    if meta.len() > cap {
        return Err(Error::Size);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(cap + 1).read_to_end(&mut bytes))
        .map_err(|_| Error::Storage)?;
    if bytes.len() as u64 > cap {
        return Err(Error::Size);
    }
    Ok(bytes)
}
