//! Actual say dispatches and nullable first-attempt wake measurements.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub step: usize,
    pub iteration: u64,
    pub attempts: u8,
    pub first_try_woke: Option<bool>,
}

fn summary(rows: &[Attempt]) -> Result<Value, String> {
    let eligible = rows.iter().filter(|row| row.first_try_woke.is_some()).count();
    let wakes = rows.iter().filter(|row| row.first_try_woke == Some(true)).count();
    let replays = rows.iter().filter(|row| row.attempts == 2).count();
    let rate = if eligible == 0 {
        None
    } else {
        let numerator = u32::try_from(wakes).map_err(|_| "talkback: too many wakes")?;
        let denominator = u32::try_from(eligible).map_err(|_| "talkback: too many eligible steps")?;
        Some(f64::from(numerator) / f64::from(denominator))
    };
    Ok(json!({"eligible_steps":eligible,"first_try_wakes":wakes,
        "first_try_wake_rate":rate,"replay_count":replays}))
}

pub(super) fn publish(report: &mut Value, rows: &[Attempt]) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    report["step_attempts"] = serde_json::to_value(rows).map_err(|_| "talkback: cannot encode attempts")?;
    report["talkback"] = summary(rows)?;
    Ok(())
}

pub(super) fn validate(report: &Value) -> Result<(), String> {
    match (report.get("step_attempts"), report.get("talkback")) {
        (None, None) => Ok(()),
        (Some(rows), Some(actual)) => {
            let rows: Vec<Attempt> = serde_json::from_value(rows.clone()).map_err(|_| "talkback: invalid attempts")?;
            if rows.iter().any(|row| !(1..=2).contains(&row.attempts)) || actual != &summary(&rows)? {
                return Err("talkback: summary does not match actual attempts".into());
            }
            Ok(())
        }
        _ => Err("talkback: attempts and summary must be published together".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab_run::run_report::Artifacts;

    fn row(step: usize, attempts: u8, woke: Option<bool>) -> Attempt {
        Attempt {
            step,
            iteration: 0,
            attempts,
            first_try_woke: woke,
        }
    }

    fn report(rows: Vec<Attempt>) -> Value {
        let root = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::create(root.path().join("run")).unwrap();
        artifacts.step_attempts = rows;
        artifacts
            .publish("B0", "T3", "synthetic", Some("synthetic failure"), false)
            .unwrap();
        serde_json::from_slice(&std::fs::read(artifacts.root.join("report.json")).unwrap()).unwrap()
    }

    #[test]
    fn ring_replay_report_counts_first_attempt_misses_and_actual_replays() {
        let rows = vec![row(0, 2, Some(false)), row(1, 1, Some(true)), row(2, 2, None)];
        let report = report(rows.clone());
        assert_eq!(report["step_attempts"], serde_json::to_value(rows).unwrap());
        assert_eq!(
            report["talkback"],
            json!({"eligible_steps":2,"first_try_wakes":1,
            "first_try_wake_rate":0.5,"replay_count":2})
        );
        assert_eq!(report["result"], "fail");
    }

    #[test]
    fn ring_replay_report_keeps_unknown_excluded_and_recovery_iterations_distinct() {
        let mut second = row(3, 2, None);
        second.iteration = 2;
        let report = report(vec![row(3, 1, None), second]);
        assert_eq!(report["step_attempts"][0]["iteration"], 0);
        assert_eq!(report["step_attempts"][1]["iteration"], 2);
        assert_eq!(
            report["talkback"],
            json!({"eligible_steps":0,"first_try_wakes":0,
            "first_try_wake_rate":null,"replay_count":1})
        );
    }

    #[test]
    fn ring_replay_report_validates_legacy_and_rejects_malformed_or_inconsistent_additions() {
        let legacy = report(vec![]);
        assert!(legacy.get("step_attempts").is_none());
        assert!(legacy.get("talkback").is_none());
        super::super::validate(&legacy).unwrap();
        let valid = report(vec![row(0, 1, Some(true))]);
        for (pointer, value) in [
            ("/step_attempts/0/attempts", json!(3)),
            ("/step_attempts/0/attempts", json!(0)),
            ("/step_attempts/0/first_try_woke", json!("unknown")),
            ("/step_attempts/0/iteration", json!(-1)),
            ("/talkback/first_try_wake_rate", json!(0.5)),
            ("/talkback/eligible_steps", json!(2)),
            ("/talkback/first_try_wakes", json!(0)),
            ("/talkback/replay_count", json!(1)),
        ] {
            let mut invalid = valid.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(super::super::validate(&invalid).is_err(), "{pointer}");
        }
        for key in ["step_attempts", "talkback"] {
            let mut invalid = valid.clone();
            invalid.as_object_mut().unwrap().remove(key);
            assert!(super::super::validate(&invalid).is_err(), "{key}");
        }
    }
}
