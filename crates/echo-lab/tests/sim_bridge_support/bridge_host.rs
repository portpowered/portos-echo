//! Reuse the repository's prebuilt-host controller; EOF owns teardown.
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct Host {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

fn controller() -> Command {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let shell = if cfg!(windows) {
        let output = Command::new("python")
            .arg(root.join("scripts/make-posix.py"))
            .output()
            .unwrap();
        assert!(output.status.success());
        PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()).join("sh.exe")
    } else {
        PathBuf::from("sh")
    };
    let mut command = Command::new(&shell);
    if cfg!(windows) {
        let inherited = std::env::var_os("PATH").unwrap();
        let paths = std::iter::once(shell.parent().unwrap().to_path_buf()).chain(std::env::split_paths(&inherited));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }
    command.arg("tests/wyoming_host.sh").current_dir(root);
    command
}

impl Host {
    pub fn start() -> (Self, u16) {
        let mut child = controller()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut host = Self { child, input, output };
        let port = host.line().parse().unwrap();
        (host, port)
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        assert_ne!(self.output.read_line(&mut line).unwrap(), 0, "controller exited early");
        line.trim().to_owned()
    }

    pub fn wake(&mut self) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "wake").unwrap();
        input.flush().unwrap();
        assert_eq!(self.line(), "wake");
    }

    pub fn finish(mut self) {
        drop(self.input.take());
        assert_eq!(self.line(), "cleaned");
        assert_eq!(self.child.wait().unwrap().code(), Some(0));
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.wait();
    }
}
