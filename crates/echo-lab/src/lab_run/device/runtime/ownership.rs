//! Exclusive bench acquisition. Retained hard links are identity witnesses, not backups.
use super::Remote;
use std::path::Path;

#[cfg(test)]
#[path = "ownership_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct Owned {
    directory: Option<String>,
    directory_id: String,
    entries: Vec<(String, String)>,
    uncertain: bool,
    ready: bool,
}

impl Remote {
    /// Publish with no-clobber hard links, never push over a nominated destination.
    pub fn stage(&mut self, local: &Path) -> Result<(), String> {
        if self.owned.directory.is_some() || self.owned.uncertain {
            return Err("device: bench acquisition already attempted".into());
        }
        let directory = format!("{}.lab-owner", self.launcher.config);
        self.owned.uncertain = true;
        self.shell(&format!("mkdir -m 700 {directory}"))?;
        self.owned.directory = Some(directory.clone());
        let identity = self.shell(&format!("stat -c %d:%i {directory}"))?;
        let parts: Vec<_> = identity.trim().split(':').collect();
        if parts.len() != 2
            || parts
                .iter()
                .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err("device: invalid bench directory identity; coordinator recovery required".into());
        }
        self.owned.directory_id = identity.trim().into();
        self.owned.uncertain = false;
        let config = format!("{directory}/config");
        self.shell
            .adb(&["push".into(), local.to_string_lossy().into_owned(), config.clone()])?;
        self.shell.adb(&["shell".into(), format!("chown 9999:9999 {config}")])?;
        self.shell(&format!("chmod 600 {config}"))?;
        if self.shell(&format!("stat -c %u:%g:%a {config}"))?.trim() != "9999:9999:600" {
            return Err("device: unmet prerequisite: pushed bridge configuration with owner 9999:9999 and mode 0600 on the Echo".into());
        }
        let mut destinations = vec![self.launcher.config.clone(), self.launcher.trace.clone()];
        destinations.extend(
            ["lab-stdin", "lab-holder.pid", "lab-stdout.log", "lab-stderr.log"]
                .map(|name| format!("{}/{name}", self.launcher.private_dir)),
        );
        for (index, destination) in destinations.into_iter().enumerate() {
            let source = if index == 0 {
                config.clone()
            } else {
                format!("{directory}/{index}")
            };
            if index == 2 {
                self.shell(&format!("mkfifo -m 600 {source}"))?;
            } else if index != 0 {
                self.shell(&format!("sh -c 'umask 077; set -C; : > {source}'"))?;
            }
            // Record the possible link before the call: a lost acknowledgement may
            // leave it behind. Release must prove identity independently either way.
            self.owned.entries.push((source.clone(), destination.clone()));
            self.shell(&format!("ln -T {source} {destination}"))
                .map_err(|_| format!("device: bench destination collision or uncertain acquisition: {destination}"))?;
        }
        self.owned.ready = true;
        Ok(())
    }

    fn identity(source: &str, destination: &str) -> String {
        format!("test ! -L {destination} && test {source} -ef {destination}")
    }

    fn check_directory(&self) -> Result<(), String> {
        let directory = self
            .owned
            .directory
            .as_ref()
            .ok_or("device: bench directory not acquired")?;
        self.shell(&format!(
            "sh -c 'test ! -L {directory} && test \"$(stat -c %d:%i {directory})\" = {}'",
            self.owned.directory_id
        ))
        .map(|_| ())
        .map_err(|_| "device: bench directory replaced; residue retained; coordinator recovery required".into())
    }

    pub(super) fn check_owned(&self) -> Result<(), String> {
        if !self.owned.ready {
            return Err("device: bench acquisition incomplete".into());
        }
        self.check_directory()?;
        for (source, destination) in &self.owned.entries {
            self.shell(&format!("sh -c '{}'", Self::identity(source, destination)))
                .map_err(|_| "device: bench artifact replaced; coordinator recovery required")?;
        }
        Ok(())
    }

    pub(super) fn release_owned(&mut self) -> Result<(), String> {
        if self.attempted || self.owned.uncertain {
            return Err("device: uncertain bench ownership or process still running; residue retained; coordinator recovery required".into());
        }
        self.owned.ready = false;
        if self.owned.directory.is_some() {
            self.check_directory()?;
        }
        let mut failures = Vec::new();
        for (source, destination) in std::mem::take(&mut self.owned.entries) {
            let command = format!(
                "sh -c 'if test -e {destination} || test -L {destination}; then \
                {} && rm -f {destination}; fi'",
                Self::identity(&source, &destination)
            );
            failures.extend(self.shell(&command).err());
        }
        if let Some(directory) = self.owned.directory.take() {
            // No recursive deletion: only the fixed source entries in our acquired directory.
            if failures.is_empty() {
                let command = format!(
                    "rm -f {directory}/config {directory}/1 {directory}/2 \
                    {directory}/3 {directory}/4 {directory}/5 && rmdir {directory}"
                );
                failures.extend(self.shell(&format!("sh -c '{command}'")).err());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "device: owned bench cleanup failed; residue retained; coordinator recovery required: {}",
                failures.join("; ")
            ))
        }
    }
}
