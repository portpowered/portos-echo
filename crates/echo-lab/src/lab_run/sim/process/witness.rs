//! Collect each bridge generation's private result after the scenario reaps its children.
use echo_bridge::world_witness::{DIRECTORY_ENV, REPORT_FILE, Report, SEED_ENV, TRACE_TICK_ENV};
use std::{
    cell::RefCell,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tokio::process::Command;

tokio::task_local! {
    // Each synchronous scenario owns its driver. Its runtimes poll bridge
    // launches on the same thread via block_on; background peers never attach.
    static CURRENT: RefCell<Checks>;
}

const MISSING: &str = "proof.availability: expected one result per bridge generation, actual missing";
const INVALID: &str = "proof.availability: expected one valid result per bridge generation, actual invalid";

impl super::Process {
    pub(in crate::lab_run::sim) fn start_bridge(command: &mut Command, observed: bool) -> Result<Self, String> {
        attach(command)?;
        Self::launch(command, observed)
    }

    pub(in crate::lab_run::sim) fn start_bridge_with_log(
        command: &mut Command,
        log: std::fs::File,
    ) -> Result<Self, String> {
        attach(command)?;
        Self::launch_with_stderr(command, log.into())
    }
}

struct Checks {
    root: tempfile::TempDir,
    generations: Vec<PathBuf>,
    seed: Option<u64>,
    trace_tick: bool,
}

pub(in crate::lab_run) fn execute(
    id: &str,
    root: &Path,
    output: &mut impl Write,
    work: impl FnOnce() -> Result<(Result<(), String>, bool), String>,
) -> Result<Result<(), String>, String> {
    let checks = match Checks::new(id) {
        Ok(checks) => checks,
        Err(error) => return Ok(emit(output, id, Err(INVALID), Err(error), false)),
    };
    CURRENT.sync_scope(RefCell::new(checks), || {
        let outcome = work();
        CURRENT.with(|current| {
            let checks = current.borrow();
            let (result, blocked) = match outcome {
                Err(error) if checks.generations.is_empty() => return Err(error),
                Err(error) => (Err(error), false),
                Ok(outcome) => outcome,
            };
            Ok(emit(output, id, checks.finish(root), result, blocked))
        })
    })
}

pub(in crate::lab_run::sim) fn attach(command: &mut Command) -> Result<(), String> {
    command
        .env_remove(DIRECTORY_ENV)
        .env_remove(SEED_ENV)
        .env_remove(TRACE_TICK_ENV);
    CURRENT
        .try_with(|checks| checks.borrow_mut().attach(command))
        .unwrap_or(Ok(()))
}

impl Checks {
    fn new(id: &str) -> Result<Self, String> {
        let seed = if id == "m2-live-connect" {
            std::env::var_os(SEED_ENV)
                .map(|seed| {
                    seed.to_str()
                        .and_then(|seed| seed.parse::<u64>().ok())
                        .ok_or("sim: invalid World seed")
                })
                .transpose()?
        } else {
            None
        };
        let temp = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "sim: cannot resolve temporary directory")?;
        let root = tempfile::Builder::new()
            .prefix("echo-world-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(temp)
            .map_err(|_| "sim: cannot own private World directory")?;
        Ok(Self {
            root,
            generations: Vec::new(),
            seed,
            trace_tick: id == "m2-live-connect" && std::env::var(TRACE_TICK_ENV).as_deref() == Ok("1"),
        })
    }

    fn attach(&mut self, command: &mut Command) -> Result<(), String> {
        let dir = self.root.path().join(self.generations.len().to_string());
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|_| "sim: cannot own private World generation")?;
        command.env(DIRECTORY_ENV, &dir);
        if let Some(seed) = self.seed {
            command.env(SEED_ENV, seed.to_string());
        }
        if self.trace_tick && self.generations.is_empty() {
            command.env(TRACE_TICK_ENV, "1");
        }
        self.generations.push(dir);
        Ok(())
    }

    fn finish(&self, artifacts: &Path) -> Result<(), &'static str> {
        let reports = collect(&self.generations)?;
        if self.seed.is_some() {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(artifacts.join("world-snapshots.private.json"))
                .map_err(|_| INVALID)?;
            serde_json::to_writer(file, &reports).map_err(|_| INVALID)?;
        }
        Ok(())
    }
}

fn collect(generations: &[PathBuf]) -> Result<Vec<Report>, &'static str> {
    if generations.is_empty() {
        return Err(MISSING);
    }
    generations.iter().map(|dir| read(dir)).collect()
}

fn read(dir: &Path) -> Result<Report, &'static str> {
    let path = dir.join(REPORT_FILE);
    let entries = fs::read_dir(dir)
        .map_err(|_| INVALID)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| INVALID)?;
    match entries.as_slice() {
        [] => return Err(MISSING),
        [entry] if entry.path() == path => (),
        _ => return Err(INVALID),
    }
    if !fs::symlink_metadata(&path).map_err(|_| INVALID)?.is_file() {
        return Err(INVALID);
    }
    let file = fs::File::open(path).map_err(|_| INVALID)?;
    let report: Report = serde_json::from_reader(file).map_err(|_| INVALID)?;
    match report.verdict().map_err(|_| INVALID)? {
        None => Ok(report),
        Some(diff) => Err(diff),
    }
}

fn emit(
    output: &mut impl Write,
    id: &str,
    world: Result<(), &'static str>,
    result: Result<(), String>,
    blocked: bool,
) -> Result<(), String> {
    let status = if world.is_ok() { "pass" } else { "fail" };
    let write_error = |_| "run stdout: write failed";
    writeln!(output, "world invariant scenario={} status={status}", id.escape_debug()).map_err(write_error)?;
    if let Err(diff) = world {
        writeln!(output, "World diff: {diff}").map_err(write_error)?;
        result.and(Err("sim: World invariant failed".into()))
    } else {
        if result.is_err() || blocked {
            writeln!(output, "World diff: none (invariants hold)").map_err(write_error)?;
        }
        result
    }
}

#[cfg(test)]
mod tests;
