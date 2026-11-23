use std::{
    cell::Cell,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use echo_boot::Result;

use super::files::{self, DEVICE, Files, ROOT};

const COMPAT: &str = "/data/local/hassmic";
const STOCK: &str = "/system/local/models/keyword/en-US/COMPUTER";
const COMPUTER: &str = "computer-en-US";

pub(super) struct Wake {
    created: Vec<PathBuf>,
    stock: Option<std::fs::File>,
    published_selection: Cell<bool>,
}

impl Wake {
    pub(super) fn plan(files: &Files<'_>) -> Result<Self> {
        let compat = files.path(COMPAT);
        let models = compat.join("models");
        let state = compat.join("state");
        let computer = models.join(COMPUTER);
        let selection = state.join("wake_word");
        for directory in [&compat, &models] {
            if files::exists(directory)? {
                files.protected(directory)?;
            }
        }
        if files::exists(&state)? {
            let metadata = files::open_dir(&state)?
                .metadata()
                .map_err(|_| "inspect A1 wake state")?;
            if (metadata.uid() != DEVICE.0 && metadata.uid() != files.root_uid) || files::mode(&metadata) != 0o700 {
                return Err("unsafe A1 wake state");
            }
        }
        let entries = [&compat, &models, &computer, &state, &selection];
        let mut created = Vec::new();
        for path in entries {
            if files::exists(path)? {
                if path != &selection
                    && !fs::symlink_metadata(path)
                        .map_err(|_| "inspect A1 wake directory")?
                        .is_dir()
                {
                    return Err("unsafe A1 wake directory");
                }
            } else {
                created.push(path.clone());
            }
        }
        let stock = if created.contains(&computer) {
            let stock = files.protected(&files.path(STOCK))?;
            manifest(&stock)?;
            Some(stock)
        } else {
            manifest(&files::open_dir(&computer)?)?;
            None
        };
        if !created.contains(&selection) {
            validate_selection(&state, &models)?;
        }
        Ok(Self {
            created,
            stock,
            published_selection: Cell::new(false),
        })
    }

    pub(super) fn apply(&self, files: &Files<'_>, temporary: &Path) -> Result<()> {
        for target in &self.created {
            match target.file_name().and_then(|name| name.to_str()) {
                Some(COMPUTER) => {
                    let source = self.stock.as_ref().ok_or("stock Computer model unavailable")?;
                    files::copy_tree(files, source, temporary, ROOT, 0o755, 12)?;
                    files::rename(temporary, target)?;
                }
                Some("state") => files.directory(target, DEVICE, 0o700)?,
                Some("wake_word") => self.write_selection(files, temporary, target)?,
                _ => files.directory(target, ROOT, 0o755)?,
            }
        }
        Ok(())
    }

    fn write_selection(&self, files: &Files<'_>, temporary: &Path, target: &Path) -> Result<()> {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        let staged = temporary.with_file_name("wake-word");
        files.write(&staged, b"computer-en-US\n", DEVICE, 0o600)?;
        let state = files::open_dir(target.parent().ok_or("missing A1 state directory")?)?;
        renameat_with(CWD, &staged, &state, "wake_word", RenameFlags::NOREPLACE)
            .map_err(|_| "publish A1 wake selection")?;
        self.published_selection.set(true);
        state.sync_all().map_err(|_| "sync A1 wake selection")
    }

    pub(super) fn undo(&self) -> Result<()> {
        for path in self.created.iter().rev() {
            if path.ends_with("wake_word") && !self.published_selection.get() {
                continue;
            }
            files::remove(path)?;
        }
        Ok(())
    }
}

fn validate_selection(state: &Path, models: &Path) -> Result<()> {
    let file = files::read_at(&files::open_dir(state)?, Path::new("wake_word"))?;
    let selection = files::read_text(file, 512)?;
    for id in selection.split_whitespace() {
        if !id
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(byte))
        {
            return Err("invalid A1 wake selection");
        }
        if id != "alexa" && id != "alexa-en-US" {
            manifest(&files::open_dir(&models.join(id))?)?;
        }
    }
    if selection.trim().is_empty() {
        return Err("invalid A1 wake selection");
    }
    Ok(())
}

fn manifest(model: &std::fs::File) -> Result<()> {
    let manifest = files::read_at(model, Path::new("pryon.manifest"))?;
    if manifest.metadata().map_err(|_| "inspect A1 wake model")?.len() == 0 {
        return Err("empty A1 wake model");
    }
    Ok(())
}
