mod calibrate_decode;
mod calibrate_fit;
pub(crate) mod calibrate_options;
mod calibrate_report;

use crate::lab_probe::{probe_command, probe_parse::probe_inputs, probe_stream::Stream};
use calibrate_decode::Decoder;
use calibrate_options::{Deadline, Options};
use calibrate_report::{Report, TARGETS};
use std::io::Write;
use std::process::Command;

fn prompt(output: &mut impl Write, number: usize) -> Result<(), String> {
    let target = TARGETS.get(number).ok_or("calibrate: too many targets")?;
    writeln!(
        output,
        "tap target {} of 5: {} at ({},{})",
        number + 1,
        target.name,
        target.x,
        target.y
    )
    .and_then(|()| output.flush())
    .map_err(|_| "calibrate: stdout write failed".into())
}

fn read(command: &mut Command, deadline: &mut impl Deadline) -> Result<String, String> {
    let mut stream = Stream::start(command)?;
    let mut bytes = Vec::new();
    while let Some(line) = stream.next(deadline.remaining(0), 0)? {
        if bytes.len() + line.len() > 1_048_576 {
            return Err("calibrate: inventory too large".into());
        }
        bytes.extend(line);
    }
    stream.exited_successfully(|| deadline.remaining(0))?;
    String::from_utf8(bytes).map_err(|_| "calibrate: invalid adb text".into())
}

fn capture(
    report: &mut Report,
    command: &mut Command,
    path: &std::path::Path,
    deadline: &mut impl Deadline,
    output: &mut impl Write,
) -> Result<(), String> {
    let mut log = std::fs::File::create(path.join("getevent.log")).map_err(|_| "calibrate: log write failed")?;
    prompt(output, 0)?;
    let stream = Stream::start(command)?;
    let mut decoder = Decoder::default();
    let mut count = 0;
    loop {
        let line = stream
            .next(deadline.remaining(count), count)?
            .ok_or("calibrate: event stream ended before five taps")?;
        log.write_all(&line)
            .and_then(|()| log.flush())
            .map_err(|_| "calibrate: log write failed")?;
        let text = std::str::from_utf8(&line).map_err(|_| "calibrate: invalid event text")?;
        if let Some(point) = decoder.line(text)? {
            let previous = count;
            count = count.max(report.add(point)?);
            if count == 5 {
                return Ok(());
            }
            if count > previous {
                prompt(output, count)?;
            }
        }
    }
}

// Keep process/clock injection crate-private; the CLI always uses the shared gate.
pub(crate) fn run(
    args: &[String],
    deadline: &mut impl Deadline,
    gate: impl FnOnce(&Options, &mut dyn FnMut(&[&str]) -> Result<String, String>) -> Result<String, String>,
    configure: impl Fn(&mut Command),
    output: &mut impl Write,
) -> Result<(), String> {
    let options = Options::parse(args)?;
    std::fs::create_dir(&options.out).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            "calibrate: output destination already exists".to_owned()
        } else {
            "calibrate: cannot create output directory".to_owned()
        }
    })?;
    let command = |arguments: &[&str]| {
        let mut command = probe_command::command(&options.adb, options.serial.as_deref(), arguments);
        configure(&mut command);
        command
    };
    let firmware_build = gate(&options, &mut |arguments| read(&mut command(arguments), deadline))?;
    let devices = probe_inputs::touch_devices(&read(&mut command(&["getevent", "-lp"]), deadline)?)?;
    let mut report = Report {
        firmware_build,
        devices,
        taps: Vec::new(),
        glass: "none".into(),
        fit: None,
    };
    let capture_result = capture(
        &mut report,
        &mut command(&["getevent", "-lt"]),
        &options.out,
        deadline,
        output,
    );
    let fit_result = report.finish();
    report.publish(&options.out)?;
    let residual = report
        .fit
        .as_ref()
        .map_or_else(|| "none".into(), |f| f.max_residual_px.to_string());
    writeln!(output, "calibrate: glass={} max_residual_px={residual}", report.glass)
        .map_err(|_| "calibrate: stdout write failed")?;
    capture_result.and(fit_result)
}

/// Production entry point: explicit adb selection and one host-monotonic deadline.
pub(crate) fn execute(args: &[String], output: &mut impl Write, errors: &mut impl Write) -> u8 {
    let result = Options::parse(args).and_then(|options| {
        let mut clock = calibrate_options::Clock::new(options.timeout);
        run(
            args,
            &mut clock,
            |_, read| crate::lab_gate::firmware(|name| read(&["getprop", name]).map(|text| text.trim().to_owned())),
            |_| {},
            output,
        )
    });
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(errors, "{error}");
            1
        }
    }
}
