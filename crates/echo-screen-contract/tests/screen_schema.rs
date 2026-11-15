//! T0: generated schemas are the published contract, compared without writes.
use echo_screen_contract::{ScreenFrame, UiAction, UiState};
use schemars::{JsonSchema, schema_for};
use std::{env, error::Error, fs, path::Path};

fn snapshot<T: JsonSchema>(name: &str) -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/screen")
        .join(format!("{name}.json"));
    let generated = format!("{}\n", serde_json::to_string_pretty(&schema_for!(T))?);
    if env::var("UPDATE_SCHEMAS").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().ok_or("snapshot has no parent")?)?;
        fs::write(&path, &generated)?;
    }
    let committed = fs::read_to_string(&path)
        .map_err(|error| format!("{}: {error}; use UPDATE_SCHEMAS=1 to update", path.display()))?;
    assert!(committed == generated, "schema mismatch: {}", path.display());
    Ok(())
}

#[test]
fn schema_snapshots() -> Result<(), Box<dyn Error>> {
    snapshot::<UiState>("ui_state")?;
    snapshot::<UiAction>("ui_action")?;
    snapshot::<ScreenFrame>("screen_frame")
}
