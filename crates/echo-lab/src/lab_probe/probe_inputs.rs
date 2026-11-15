use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn quoted(line: &str) -> Result<String, String> {
    let start = line.find('"').ok_or("probe input devices: missing quoted name")?;
    let value = line[start + 1..]
        .strip_suffix('"')
        .filter(|s| !s.is_empty())
        .ok_or("probe input devices: malformed name")?;
    Ok(value.to_owned())
}

fn axis(line: &str) -> Result<Option<String>, String> {
    let label = if line.contains("ABS_MT_POSITION_X") || line.contains("ABS_X") {
        "X"
    } else if line.contains("ABS_MT_POSITION_Y") || line.contains("ABS_Y") {
        "Y"
    } else {
        return Ok(None);
    };
    let bound = |key: &str| -> Result<i64, String> {
        line.split_once(key)
            .and_then(|(_, tail)| tail.split([',', ' ']).find(|v| !v.is_empty()))
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| "probe getevent: malformed axis range".into())
    };
    let min = bound("min ")?;
    let max = bound("max ")?;
    if min > max {
        return Err("probe getevent: reversed axis range".into());
    }
    Ok(Some(format!("{label}={min}..{max}")))
}

fn events(text: &str, devices: &mut BTreeMap<String, BTreeSet<String>>) -> Result<(), String> {
    let mut current = None;
    let mut unnamed = false;
    for line in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if line.starts_with("add device") {
            if unnamed {
                return Err("probe getevent: missing device name".into());
            }
            current = None;
            unnamed = true;
        }
        if line.starts_with("name:") {
            let name = quoted(line)?;
            devices.entry(name.clone()).or_default();
            current = Some(name);
            unnamed = false;
        } else if let Some(name) = &current {
            let capabilities = devices.get_mut(name).ok_or("probe getevent: missing device")?;
            for kind in [
                "EV_SYN", "EV_KEY", "EV_REL", "EV_ABS", "EV_MSC", "EV_SW", "EV_LED", "EV_REP", "EV_FF", "EV_PWR",
            ] {
                if line.contains(kind) || line.starts_with(&format!("{} (", &kind[3..])) {
                    capabilities.insert(kind.to_owned());
                }
            }
            if let Some(range) = axis(line)? {
                capabilities.insert(range);
            }
        }
    }
    if unnamed || (!text.trim().is_empty() && devices.is_empty()) {
        return Err("probe getevent: no named devices in nonempty observation".into());
    }
    Ok(())
}

pub fn parse(getevent: &str, proc_input: &str) -> Result<Vec<Value>, String> {
    let mut devices = BTreeMap::<String, BTreeSet<String>>::new();
    events(getevent, &mut devices)?;
    let mut current = None;
    let mut proc_names = 0;
    for line in proc_input.lines().map(str::trim) {
        if line.is_empty() {
            current = None;
        }
        if line.starts_with("N: Name=") {
            let name = quoted(line)?;
            devices.entry(name.clone()).or_default();
            current = Some(name);
            proc_names += 1;
        } else if let Some(name) = &current
            && let Some(bits) = line.strip_prefix("B: ")
        {
            devices
                .get_mut(name)
                .ok_or("probe proc input: missing device")?
                .insert(bits.to_owned());
        }
    }
    if !proc_input.trim().is_empty() && proc_names == 0 {
        return Err("probe proc input: no named devices in nonempty observation".into());
    }
    Ok(devices
        .into_iter()
        .map(|(name, capabilities)| json!({"name": name, "capabilities": capabilities}))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_capabilities_and_proc_only_devices_round_trip() {
        let value = parse("add device 1:\nname: \"touch\"\nABS (0003): ABS_X : min 0, max 12\nABS_Y : min -1, max 7\nKEY (0001): BTN_TOUCH\n",
            "N: Name=\"button\"\nB: KEY=1\n\nN: Name=\"touch\"\nB: EV=b\n").unwrap();
        assert_eq!(
            value,
            vec![
                json!({"name":"button","capabilities":["KEY=1"]}),
                json!({"name":"touch","capabilities":["EV=b","EV_ABS","EV_KEY","X=0..12","Y=-1..7"]})
            ]
        );
        assert_eq!(parse("", "").unwrap(), Vec::<Value>::new());
    }

    #[test]
    fn malformed_names_ranges_and_incomplete_devices_fail_closed() {
        for text in [
            "name: missing quotes",
            "name: \"\"",
            "name: \"touch\"\nABS_X : min 2, max 1",
            "name: \"touch\"\nABS_X : max 1",
            "name: \"touch\"\nABS_Y : min 0, max bad",
            "add device 1:\nadd device 2:\nname: \"touch\"",
            "name: \"touch\"\nadd device 2:",
        ] {
            assert!(parse(text, "").is_err(), "{text}");
        }
        assert!(parse("", "N: Name=missing quotes").is_err());
        assert!(parse("", "garbage").is_err());
    }
}
