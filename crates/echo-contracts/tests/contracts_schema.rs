//! T0: generated schemas are the published contract, compared without writes.
use echo_contracts::{ids::*, live::*, secret::Secret, task::TaskEvent, wyoming::*};
use schemars::{JsonSchema, schema_for};
use std::{env, error::Error, fs, path::Path};

fn compare(path: &Path, generated: &str, update: bool) -> Result<(), Box<dyn Error>> {
    if update {
        fs::create_dir_all(path.parent().ok_or("snapshot has no parent")?)?;
        fs::write(path, generated)?;
    }
    let committed = fs::read_to_string(path)
        .map_err(|error| format!("{}: {error}; use UPDATE_SCHEMAS=1 to update", path.display()))?;
    if committed != generated {
        return Err(format!("schema mismatch: {}", path.display()).into());
    }
    Ok(())
}

fn snapshot<T: JsonSchema>(name: &str) -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/contracts")
        .join(format!("{name}.json"));
    let generated = format!("{}\n", serde_json::to_string_pretty(&schema_for!(T))?);
    compare(&path, &generated, env::var("UPDATE_SCHEMAS").as_deref() == Ok("1"))
}

#[test]
fn schema_snapshots() -> Result<(), Box<dyn Error>> {
    snapshot::<SessionId>("session_id")?;
    snapshot::<CallId>("call_id")?;
    snapshot::<TurnId>("turn_id")?;
    snapshot::<DelegationId>("delegation_id")?;
    snapshot::<TaskId>("task_id")?;
    snapshot::<Secret<String>>("secret_string")?;
    snapshot::<Header>("header")?;
    snapshot::<Event>("event")?;
    snapshot::<AudioFormat>("audio_format")?;
    snapshot::<AudioChunkData>("audio_chunk_data")?;
    snapshot::<RunPipelineData>("run_pipeline_data")?;
    snapshot::<WyomingEvent>("wyoming_event")?;
    snapshot::<TaskEvent>("task_event")?;
    snapshot::<DelegationTarget>("delegation_target")?;
    snapshot::<Delegation>("delegation")?;
    snapshot::<LiveEvent>("live_event")?;
    Ok(())
}

#[test]
fn snapshot_update_and_failure_diagnostics() -> Result<(), Box<dyn Error>> {
    let directory = env::temp_dir().join(format!("contracts-snapshot-test-{}", std::process::id()));
    fs::create_dir(&directory)?;
    let path = directory.join("nested/task_event.json");
    let generated = format!("{}\n", serde_json::to_string_pretty(&schema_for!(TaskEvent))?);
    let result = (|| -> Result<(), Box<dyn Error>> {
        let missing = compare(&path, &generated, false)
            .err()
            .ok_or("missing snapshot accepted")?;
        assert!(missing.to_string().contains("task_event.json"));
        assert!(missing.to_string().contains("UPDATE_SCHEMAS=1"));
        assert!(!path.exists(), "default comparison wrote a missing snapshot");
        compare(&path, &generated, true)?;
        assert_eq!(fs::read_to_string(&path)?, generated);
        fs::write(&path, "stale\n")?;
        let drift = compare(&path, &generated, false)
            .err()
            .ok_or("stale snapshot accepted")?;
        assert!(drift.to_string().contains("schema mismatch:"));
        assert!(drift.to_string().contains("task_event.json"));
        assert_eq!(fs::read_to_string(&path)?, "stale\n");
        compare(&path, &generated, true)?;
        compare(&path, &generated, false)?;
        assert_eq!(fs::read_to_string(&path)?, generated);
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(&directory);
    result?;
    cleanup?;
    Ok(())
}
