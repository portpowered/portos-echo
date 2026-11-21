#[cfg(unix)]
use super::Directory;
use super::{BridgeConfig, MINIMAL, SettingsOverlay, overlay};

#[test]
fn screen_settings_muse_overlay_masks_base_and_refuses_unsafe_references() {
    let base = BridgeConfig::from_toml_str(&format!(
        "{MINIMAL}\n[muse]\nurl='https://127.0.0.1:1'\nca_file='owner.der'\ncredential_file='owner-key'\n"
    ))
    .unwrap();
    assert_eq!(SettingsOverlay::default().apply_to(&base).unwrap().muse, base.muse);
    let clear = overlay("[muse]\nenabled=false");
    assert_eq!(clear.apply_to(&base).unwrap().muse, None);
    assert_eq!(clear.to_toml_string().unwrap(), "[muse]\nenabled = false\n");
    for ca in [
        "/muse/connection-1/ca.der",
        "muse/../connection-1/ca.der",
        "muse//connection-1/ca.der",
        "muse/connection-2/ca.der",
        "muse/connection-1/../ca.der",
    ] {
        let text = format!(
            "[muse]\nurl='https://127.0.0.1:1'\nca_file='{ca}'\ncredential_file='muse/connection-1/credential'\n"
        );
        assert_eq!(
            SettingsOverlay::from_toml_str(&text).unwrap_err().to_string(),
            "muse: expected private connection files under home"
        );
    }
    for input in [
        "[muse]\nenabled=true",
        "[muse]\nenabled=false\ncredential_file='outside'",
    ] {
        assert!(SettingsOverlay::from_toml_str(input).is_err());
    }
}

#[cfg(unix)]
#[test]
fn screen_settings_muse_overlay_rejects_symlinked_generation_and_file() {
    use std::{fs, os::unix::fs::symlink};
    let root = Directory::new().unwrap();
    let mut base = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    let home = root.0.join("home");
    let owned = home.join("muse/connection-1");
    let outside = root.0.join("outside");
    fs::create_dir_all(owned.parent().unwrap()).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("ca.der"), b"fixture").unwrap();
    fs::write(outside.join("credential"), b"fixture").unwrap();
    base.home = Some(echo_config::HomeConfig {
        dir: home,
        agent_tools: echo_config::AgentTools::default(),
    });
    let saved = overlay(
        "[muse]\nurl='https://127.0.0.1:1'\nca_file='muse/connection-1/ca.der'\ncredential_file='muse/connection-1/credential'\n",
    );
    symlink(&outside, &owned).unwrap();
    assert!(saved.apply_to(&base).is_err());
    fs::remove_file(&owned).unwrap();
    fs::create_dir(&owned).unwrap();
    fs::write(owned.join("ca.der"), b"fixture").unwrap();
    symlink(outside.join("credential"), owned.join("credential")).unwrap();
    assert!(saved.apply_to(&base).is_err());
    fs::remove_file(owned.join("credential")).unwrap();
    fs::write(owned.join("credential"), b"fixture").unwrap();
    assert_eq!(
        saved.apply_to(&base).unwrap().muse.unwrap().ca_file,
        owned.join("ca.der")
    );
}
