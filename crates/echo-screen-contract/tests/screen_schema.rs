//! T0: generated schemas are the published contract, compared without writes.
use echo_screen_contract::mouth::MouthBatch;
use echo_screen_contract::settings_v1::{
    PromptSaveV1, SaveResultV1, SettingsErrorV1, SettingsPatchV1, SettingsSnapshotV1,
};
use echo_screen_contract::settings_v2::{
    MuseProbeRequestV2, MuseProbeResultV2, OpenAiAccountV2, OpenAiApiKeyRequestV2, PortosOperationStartedV2,
    PortosOperationV2, SaveResultV2, SettingsErrorV2, SettingsPatchV2, SettingsSnapshotV2,
};
use echo_screen_contract::{ScreenFrame, UiAction, UiState};
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
        .join("../../schemas/screen")
        .join(format!("{name}.json"));
    let generated = format!("{}\n", serde_json::to_string_pretty(&schema_for!(T))?);
    compare(&path, &generated, env::var("UPDATE_SCHEMAS").as_deref() == Ok("1"))
}

#[test]
fn schema_snapshots() -> Result<(), Box<dyn Error>> {
    snapshot::<UiState>("ui_state")?;
    snapshot::<UiAction>("ui_action")?;
    snapshot::<ScreenFrame>("screen_frame")?;
    snapshot::<MouthBatch>("mouth_batch")
}

#[test]
fn settings_schema_snapshots() -> Result<(), Box<dyn Error>> {
    snapshot::<SettingsSnapshotV1>("settings_snapshot_v1")?;
    snapshot::<SettingsPatchV1>("settings_patch_v1")?;
    snapshot::<PromptSaveV1>("prompt_save_v1")?;
    snapshot::<SaveResultV1>("save_result_v1")?;
    snapshot::<SettingsErrorV1>("settings_error_v1")
}

#[test]
fn settings_v2_schema_snapshots() -> Result<(), Box<dyn Error>> {
    snapshot::<SettingsSnapshotV2>("settings_snapshot_v2")?;
    snapshot::<SettingsPatchV2>("settings_patch_v2")?;
    snapshot::<SaveResultV2>("save_result_v2")?;
    snapshot::<SettingsErrorV2>("settings_error_v2")?;
    snapshot::<PortosOperationV2>("portos_operation_v2")?;
    snapshot::<PortosOperationStartedV2>("portos_operation_started_v2")?;
    snapshot::<OpenAiAccountV2>("openai_account_v2")?;
    snapshot::<OpenAiApiKeyRequestV2>("openai_api_key_request_v2")?;
    snapshot::<MuseProbeRequestV2>("muse_probe_request_v2")?;
    snapshot::<MuseProbeResultV2>("muse_probe_result_v2")
}

#[test]
fn snapshot_update_and_failure_diagnostics() -> Result<(), Box<dyn Error>> {
    let directory = env::temp_dir().join(format!("screen-snapshot-test-{}", std::process::id()));
    // The name carries this process id; one that already exists is a stale leftover of an
    // interrupted earlier process that had the same id (Windows reuses ids), never a live one.
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir(&directory)?;
    let path = directory.join("nested/ui_action.json");
    let generated = format!("{}\n", serde_json::to_string_pretty(&schema_for!(UiAction))?);
    let result = (|| -> Result<(), Box<dyn Error>> {
        let missing = compare(&path, &generated, false)
            .err()
            .ok_or("missing snapshot accepted")?;
        assert!(missing.to_string().contains("ui_action.json"));
        assert!(missing.to_string().contains("UPDATE_SCHEMAS=1"));
        assert!(!path.exists(), "default comparison wrote a missing snapshot");
        compare(&path, &generated, true)?;
        assert_eq!(fs::read_to_string(&path)?, generated);
        fs::write(&path, "stale\n")?;
        let drift = compare(&path, &generated, false)
            .err()
            .ok_or("stale snapshot accepted")?;
        assert!(drift.to_string().contains("schema mismatch:"));
        assert!(drift.to_string().contains("ui_action.json"));
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
