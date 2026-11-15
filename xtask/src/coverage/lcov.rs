use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct File {
    pub lines: BTreeMap<u32, bool>,
    pub branches: BTreeMap<(u32, u32, u32), bool>,
}

pub type Report = BTreeMap<String, File>;

fn number(value: &str) -> Result<u32, String> {
    value.parse().map_err(|_| format!("invalid LCOV number {value:?}"))
}

fn record(file: &mut File, line: &str) -> Result<(), String> {
    if let Some(data) = line.strip_prefix("DA:") {
        let parts: Vec<_> = data.split(',').collect();
        if !(2..=3).contains(&parts.len()) {
            return Err("invalid LCOV DA record".into());
        }
        let hit = number(parts[1])? > 0;
        *file.lines.entry(number(parts[0])?).or_default() |= hit;
    } else if let Some(data) = line.strip_prefix("BRDA:") {
        let parts: Vec<_> = data.split(',').collect();
        if parts.len() != 4 {
            return Err("invalid LCOV BRDA record".into());
        }
        let key = (number(parts[0])?, number(parts[1])?, number(parts[2])?);
        let hit = parts[3] != "-" && number(parts[3])? > 0;
        *file.branches.entry(key).or_default() |= hit;
    } else if !["TN:", "FN:", "FNDA:", "FNF:", "FNH:", "LF:", "LH:", "BRF:", "BRH:"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
    {
        return Err(format!("unsupported LCOV record {line:?}"));
    }
    Ok(())
}

pub fn parse(input: &str, root: &str) -> Result<Report, String> {
    let mut report = Report::new();
    let mut current = None;
    for line in input.lines().filter(|line| !line.is_empty()) {
        if let Some(path) = line.strip_prefix("SF:") {
            if current.is_some() {
                return Err("LCOV record missing end_of_record".into());
            }
            let path = path.replace('\\', "/");
            let relative = path.strip_prefix(&format!("{root}/")).unwrap_or(&path);
            if relative.is_empty()
                || relative.contains(':')
                || relative.starts_with('/')
                || relative.split('/').any(|part| part == "..")
            {
                return Err(format!("LCOV source outside workspace: {path}"));
            }
            current = Some(relative.to_owned());
        } else if line == "end_of_record" {
            if current.take().is_none() {
                return Err("LCOV end_of_record without source".into());
            }
        } else if !line.starts_with("TN:") {
            let path = current.as_ref().ok_or("LCOV data without source")?;
            record(report.entry(path.clone()).or_default(), line)?;
        }
    }
    if current.is_some() || report.is_empty() || report.values().all(|file| file.lines.is_empty()) {
        return Err("LCOV missing complete executable source evidence".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_binary_records_by_source_line_and_branch_identity() {
        let report = parse("TN:\nSF:C:\\repo\\a.rs\nDA:1,0\nBRDA:1,0,0,-\nend_of_record\nSF:C:/repo/a.rs\nDA:1,2\nDA:2,0,checksum\nBRDA:1,0,0,1\nBRDA:1,0,1,0\nend_of_record\n", "C:/repo").unwrap(); // lint-rules: allow-literal LCOV parser coverage assertion fixture
        assert_eq!(report["a.rs"].lines, BTreeMap::from([(1, true), (2, false)]));
        assert_eq!(
            report["a.rs"].branches,
            BTreeMap::from([((1, 0, 0), true), ((1, 0, 1), false)])
        );
    }

    #[test]
    fn malformed_missing_and_external_evidence_fails_closed() {
        for input in [
            "",
            "DA:1,1", // lint-rules: allow-literal LCOV parser coverage assertion fixture
            "end_of_record",
            "SF:a.rs\nDA:1,1", // lint-rules: allow-literal LCOV parser coverage assertion fixture
            "SF:a.rs\nSF:b.rs",
            "SF:/else/a.rs",
            "SF:../a.rs",
            "SF:\n",
            "SF:a.rs\nDA:1\n", // lint-rules: allow-literal LCOV parser coverage assertion fixture
            "SF:a.rs\nDA:x,1\n",
            "SF:a.rs\nBRDA:1,0\n", // lint-rules: allow-literal LCOV parser coverage assertion fixture
            "SF:a.rs\nunknown\n",
            "SF:a.rs\nend_of_record\n",
        ] {
            assert!(parse(input, "/repo").is_err(), "accepted {input}");
        }
        let report = parse(
            "SF:a.rs\nFN:1,f\nFNDA:1,f\nFNF:1\nFNH:1\nDA:1,1\nLF:1\nLH:1\nBRF:0\nBRH:0\nend_of_record", // lint-rules: allow-literal LCOV parser coverage assertion fixture
            "/repo",
        )
        .unwrap();
        assert_eq!(report["a.rs"].lines.len(), 1);
    }
}
