use std::{
    fs,
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
}

impl Wake {
    pub(super) fn plan(files: &Files<'_>) -> Result<Self> {
        let compat = files.path(COMPAT);
        let models = compat.join("models");
        let state = compat.join("state");
        let computer = models.join(COMPUTER);
        let selection = state.join("wake_word");
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
        Ok(Self { created, stock })
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
                Some("wake_word") => files.write(target, b"computer-en-US\n", DEVICE, 0o600)?,
                _ => files.directory(target, ROOT, 0o755)?,
            }
        }
        Ok(())
    }

    pub(super) fn undo(&self) -> Result<()> {
        for path in self.created.iter().rev() {
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
