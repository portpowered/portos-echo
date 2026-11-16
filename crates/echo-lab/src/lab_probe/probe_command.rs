use std::path::Path;
use std::process::Command;

pub(crate) fn command(adb: &Path, serial: Option<&str>, read: &[&str]) -> Command {
    let mut command = Command::new(adb);
    if let Some(serial) = serial {
        command.args(["-s", serial]);
    }
    command.arg("shell").args(read);
    command
}
