use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use echo_boot::{
    Result,
    alexa::{Journal, PACKAGES, PackageState, SETTINGS},
    layout,
};
use tokio::runtime::{Builder, Runtime};

use super::{command_io, firewall_io::FirewallIo, layout_io};

const SCREEN: &str = "org.portos.echoscreen";
const ANDROID_LIMIT: Duration = Duration::from_secs(10);
const INSTALL_LIMIT: Duration = Duration::from_secs(30);
const JOURNAL_LIMIT: u64 = 16 * 1024;

pub(super) struct DeviceIo {
    pub(super) commands: FirewallIo,
    pub(super) runtime: Runtime,
    pub(super) journal: PathBuf,
    pub(super) apk: PathBuf,
}

impl DeviceIo {
    pub(super) fn open() -> Result<Self> {
        Ok(Self {
            commands: FirewallIo::default(),
            runtime: Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "open device service runtime")?,
            journal: layout_io::boot_state()?.join(layout::ALEXA_STATE),
            apk: Path::new(layout::ROOT).join(layout::BIN).join(layout::SCREEN_APK),
        })
    }

    pub(super) fn prepare(&self, mut log: impl FnMut(&'static str)) {
        if let Err(stage) = self.take_over(&mut log) {
            log(stage);
        }
        self.screen(log);
    }

    fn command(&self, name: &str, args: &[&str], stage: &'static str) -> Result<String> {
        let tool = self.commands.find(name)?.ok_or(stage)?;
        command_io::run_bounded(&self.runtime, &tool, args, None, stage, ANDROID_LIMIT)
    }

    fn load(&self) -> Result<Option<Journal>> {
        layout_io::read_journal(&self.journal, JOURNAL_LIMIT)?
            .map(|bytes| Journal::decode(&bytes))
            .transpose()
    }

    fn save(&self, journal: &Journal) -> Result<()> {
        let bytes = journal.encode()?;
        if bytes.len() as u64 > JOURNAL_LIMIT {
            return Err("Alexa journal too large");
        }
        layout_io::write_journal(&self.journal, &bytes)
    }

    fn take_over(&self, log: &mut impl FnMut(&'static str)) -> Result<()> {
        let mut journal = self.load()?.unwrap_or_default();
        if let Err(stage) = self.disable_alexa(&mut journal, log) {
            log(stage);
        }
        if journal.wifi_diagnostics_restore_pending != Some(true) {
            journal.wifi_diagnostics_restore_pending = Some(true);
            self.save(&journal)?;
        }
        self.wifi_diagnostics(false)
    }

    fn disable_alexa(&self, journal: &mut Journal, log: &mut impl FnMut(&'static str)) -> Result<()> {
        let enabled = self.command(
            "pm",
            &["list", "packages", "--user", "0", "-e"],
            "list enabled Alexa packages",
        )?;
        for &name in PACKAGES {
            if listed(&enabled, name)
                && let Err(stage) = self.disable_package(journal, name)
            {
                log(stage);
            }
        }
        for &(namespace, key, wanted) in SETTINGS {
            if let Err(stage) = self.apply_setting(journal, namespace, key, wanted) {
                log(stage);
            }
        }
        Ok(())
    }

    fn disable_package(&self, journal: &mut Journal, name: &str) -> Result<()> {
        let previous = match self.package_state(name)? {
            0 => PackageState::Default,
            1 => PackageState::Enabled,
            _ => return Ok(()),
        };
        journal.packages.entry(name.to_owned()).or_insert(previous);
        self.save(journal)?;
        self.change_package(name, "disable-user", 3)
    }

    fn package_state(&self, name: &str) -> Result<u8> {
        const STAGE: &str = "read Alexa package state";
        let dump = self.command("dumpsys", &["package", name], STAGE)?;
        let user = dump
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("User 0:"))
            .ok_or(STAGE)?;
        user.split_ascii_whitespace()
            .find_map(|part| part.strip_prefix("enabled="))
            .and_then(|value| value.parse().ok())
            .filter(|state| *state <= 4)
            .ok_or(STAGE)
    }

    fn change_package(&self, name: &str, action: &str, expected: u8) -> Result<()> {
        const STAGE: &str = "change Alexa package state";
        self.command("pm", &[action, "--user", "0", name], STAGE)?;
        (self.package_state(name)? == expected).then_some(()).ok_or(STAGE)
    }

    fn apply_setting(&self, journal: &mut Journal, namespace: &str, key: &str, wanted: &str) -> Result<()> {
        let previous = self.setting(namespace, key)?;
        if previous.as_deref() == Some(wanted) {
            return Ok(());
        }
        journal.remember_setting(namespace, key, previous);
        self.save(journal)?;
        self.change_setting(namespace, key, Some(wanted))
    }

    fn setting(&self, namespace: &str, key: &str) -> Result<Option<String>> {
        const STAGE: &str = "read Alexa-related Android setting";
        let output = self.command("settings", &["--user", "0", "get", namespace, key], STAGE)?;
        let value = output.strip_suffix('\n').ok_or(STAGE)?;
        if value != "null" {
            return Ok(Some(value.to_owned()));
        }
        let values = self.command("settings", &["--user", "0", "list", namespace], STAGE)?;
        Ok(values
            .lines()
            .filter_map(|line| line.split_once('='))
            .find_map(|(name, value)| (name == key).then(|| value.to_owned())))
    }

    fn change_setting(&self, namespace: &str, key: &str, previous: Option<&str>) -> Result<()> {
        const STAGE: &str = "change Alexa-related Android setting";
        let mut args = vec![
            "--user",
            "0",
            if previous.is_some() { "put" } else { "delete" },
            namespace,
            key,
        ];
        args.extend(previous);
        self.command("settings", &args, STAGE)?;
        (self.setting(namespace, key)?.as_deref() == previous)
            .then_some(())
            .ok_or(STAGE)
    }

    pub(super) fn restore(&self, mut log: impl FnMut(&'static str)) -> Result<()> {
        let Some(mut journal) = self.load()? else {
            return Ok(());
        };
        let mut failed = false;
        for (name, previous) in journal.packages.clone() {
            let (action, expected) = match previous {
                PackageState::Default => ("default-state", 0),
                PackageState::Enabled => ("enable", 1),
            };
            match self.change_package(&name, action, expected) {
                Ok(()) => {
                    journal.packages.remove(&name);
                    self.save(&journal)?;
                }
                Err(stage) => {
                    log(stage);
                    failed = true;
                }
            }
        }
        let mut index = 0;
        while let Some(entry) = journal.settings.get(index) {
            match self.change_setting(&entry.namespace, &entry.key, entry.previous.as_deref()) {
                Ok(()) => {
                    journal.settings.remove(index);
                    self.save(&journal)?;
                }
                Err(stage) => {
                    log(stage);
                    failed = true;
                    index += 1;
                }
            }
        }
        if failed {
            return Err("Alexa restore incomplete");
        }
        if journal.wifi_diagnostics_restore_pending == Some(true) {
            self.wifi_diagnostics(true)?;
            journal.wifi_diagnostics_restore_pending = Some(false);
            self.save(&journal)?;
        }
        Ok(())
    }

    fn wifi_diagnostics(&self, enabled: bool) -> Result<()> {
        const STAGE: &str = "set Athena Wi-Fi diagnostics";
        let value = if enabled { "true" } else { "false" };
        let output = self.command(
            "am",
            &[
                "broadcast",
                "--user",
                "0",
                "-a",
                "com.amazon.android.service.wifidiags.SET_ENABLEWIFIDIAGS",
                "--ez",
                "val",
                value,
            ],
            STAGE,
        )?;
        output
            .lines()
            .any(|line| line.starts_with("Broadcast completed:"))
            .then_some(())
            .ok_or(STAGE)
    }

    fn screen(&self, mut log: impl FnMut(&'static str)) {
        let installed = match self.installed_apk() {
            Ok(path) => path,
            Err(stage) => {
                log(stage);
                return;
            }
        };
        let available = match self.install_screen(installed.as_deref()) {
            Ok(installed_now) => installed_now || installed.is_some(),
            Err(stage) => {
                log(stage);
                installed.is_some()
            }
        };
        if available && let Err(stage) = self.launch_screen() {
            log(stage);
        }
    }

    fn installed_apk(&self) -> Result<Option<PathBuf>> {
        const STAGE: &str = "locate installed screen APK";
        let packages = self.command("pm", &["list", "packages", "--user", "0", SCREEN], STAGE)?;
        if !listed(&packages, SCREEN) {
            return Ok(None);
        }
        let output = self.command("pm", &["path", "--user", "0", SCREEN], STAGE)?;
        let paths: Vec<_> = output
            .lines()
            .filter_map(|line| line.strip_prefix("package:"))
            .map(Path::new)
            .filter(|path| path.is_absolute())
            .collect();
        paths
            .iter()
            .copied()
            .find(|path| path.ends_with("base.apk"))
            .or_else(|| match paths.as_slice() {
                [path] if path.extension().is_some_and(|ext| ext == "apk") => Some(*path),
                _ => None,
            })
            .map(|path| Some(path.to_path_buf()))
            .ok_or(STAGE)
    }

    fn install_screen(&self, installed: Option<&Path>) -> Result<bool> {
        const STAGE: &str = "install staged screen APK";
        let Some(staged) = layout_io::apk_file(&self.apk, true)? else {
            return if installed.is_some() {
                Ok(false)
            } else {
                Err("screen APK unavailable")
            };
        };
        let digest = layout_io::hash(staged, "hash staged screen APK")?;
        if let Some(path) = installed {
            let file = layout_io::apk_file(path, false)?.ok_or("read installed screen APK")?;
            if digest == layout_io::hash(file, "hash installed screen APK")? {
                return Ok(false);
            }
        }
        let tool = self.commands.find("pm")?.ok_or(STAGE)?;
        let path = self.apk.to_str().ok_or(STAGE)?;
        let output = command_io::run_bounded(
            &self.runtime,
            &tool,
            &["install", "-r", path],
            None,
            STAGE,
            INSTALL_LIMIT,
        )?;
        output
            .lines()
            .any(|line| line == "Success")
            .then_some(true)
            .ok_or(STAGE)
    }

    fn launch_screen(&self) -> Result<()> {
        const STAGE: &str = "launch installed screen activity";
        let output = self.command(
            "am",
            &[
                "start",
                "-W",
                "--user",
                "0",
                "-n",
                "org.portos.echoscreen/.MainActivity",
            ],
            STAGE,
        )?;
        output
            .lines()
            .any(|line| line == "Status: ok")
            .then_some(())
            .ok_or(STAGE)
    }
}

fn listed(output: &str, name: &str) -> bool {
    output.lines().any(|line| line.strip_prefix("package:") == Some(name))
}

pub(super) fn log(stage: &'static str) {
    let _ = writeln!(std::io::stderr(), "echo-bridge boot: {stage}");
}
