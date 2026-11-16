/// Shared read-only admission rule; callers retain their own bounded transport.
pub(crate) fn firmware(mut property: impl FnMut(&str) -> Result<String, String>) -> Result<String, String> {
    let build = property("ro.build.version.incremental")?;
    if !include_str!("../data/probed-builds.txt")
        .lines()
        .any(|known| known == build)
    {
        return Err("firmware: build is not in probed-builds.txt; coordinator probe required".into());
    }
    let mode = property("ro.bootmode")?;
    let completed = property("sys.boot_completed")?;
    if mode != "normal" || completed != "1" {
        return Err(format!("firmware: Echo is not booted to Android (bootmode {mode})"));
    }
    Ok(build)
}

#[cfg(test)]
mod tests {
    use super::firmware;

    #[test]
    fn lab_gate_preserves_order_refusals_and_transport_errors() {
        for (build, mode, completed, expected, count) in [
            (
                "unknown",
                "normal",
                "1",
                Err("firmware: build is not in probed-builds.txt; coordinator probe required"),
                1,
            ),
            (
                "0036811211140",
                "recovery",
                "1",
                Err("firmware: Echo is not booted to Android (bootmode recovery)"),
                3,
            ),
            (
                "0036811211140",
                "normal",
                "0",
                Err("firmware: Echo is not booted to Android (bootmode normal)"),
                3,
            ),
            ("0036811211140", "normal", "1", Ok("0036811211140"), 3),
        ] {
            let mut reads = Vec::new();
            let result = firmware(|name| {
                reads.push(name.to_owned());
                Ok(match name {
                    "ro.build.version.incremental" => build,
                    "ro.bootmode" => mode,
                    "sys.boot_completed" => completed,
                    _ => unreachable!(),
                }
                .into())
            });
            assert_eq!(result.as_deref().map_err(String::as_str), expected);
            assert_eq!(
                reads,
                ["ro.build.version.incremental", "ro.bootmode", "sys.boot_completed"][..count]
            );
        }
        for failure in 0..3 {
            let mut index = 0;
            let result = firmware(|_| {
                index += 1;
                if index == failure + 1 {
                    Err("transport failed".into())
                } else {
                    Ok("0036811211140".into())
                }
            });
            assert_eq!(result, Err("transport failed".into()));
            assert_eq!(index, failure + 1);
        }
    }
}
