//! T1: pure pack configuration and sparse overlay behavior; no sockets.
use super::*;
use echo_config::SettingsOverlay;

fn configured(packs: &str) -> String {
    let home = std::env::temp_dir().join("echo-screen-pack-unopened");
    format!(
        "{}\n[home]\ndir='{}'",
        MINIMAL.replace("[screen]", &format!("[screen]\npacks={packs}")),
        home.display()
    )
}

fn download(url: &str, sha256: &str) -> String {
    format!("[{{url='{url}',sha256='{sha256}'}}]")
}

#[test]
fn screen_pack_download_defaults_and_exact_pins_are_offline() {
    assert_eq!(BridgeConfig::from_toml_str(MINIMAL).unwrap().screen.packs, []);
    let url = "https://127.0.0.1:443/template.screenpack.tar.gz?version=1";
    let digest = "aB01".repeat(16);
    let input = configured(&download(url, &digest));
    let directory = Directory::new().unwrap();
    let path = directory.write(&input).unwrap();
    let expected = BridgeConfig::from_toml_str(&input).unwrap();
    assert_eq!(expected.screen.packs.len(), 1);
    assert_eq!(expected.screen.packs[0].url, url);
    assert_eq!(expected.screen.packs[0].sha256, digest);
    assert_eq!(BridgeConfig::load(path).unwrap(), expected);
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    assert_eq!(BridgeConfig::from_toml_str(&configured("[]")).unwrap().screen.packs, []);
}

#[test]
fn screen_pack_downloads_require_home_but_empty_list_is_backwards_compatible() {
    let packs = download("https://127.0.0.1/pack.tar.gz", &"a".repeat(64));
    let input = MINIMAL.replace("[screen]", &format!("[screen]\npacks={packs}"));
    assert_eq!(
        BridgeConfig::from_toml_str(&input).unwrap_err().to_string(),
        "screen.packs: requires home.dir"
    );
    let empty = MINIMAL.replace("[screen]", "[screen]\npacks=[]");
    assert_eq!(
        BridgeConfig::from_toml_str(&empty).unwrap(),
        BridgeConfig::from_toml_str(MINIMAL).unwrap()
    );
}

#[test]
fn screen_pack_urls_refuse_insecure_or_ambiguous_authorities_without_leaking_input() {
    for url in [
        "http://127.0.0.1/pack.tar.gz",
        "https://127.0.0.1/pack.tar.gz#SYNTHETIC_VALUE",
        "https://SYNTHETIC_VALUE@127.0.0.1/pack.tar.gz",
        "https://127.0.0.1:65536/pack.tar.gz", // lint-rules: allow-literal inert invalid-port parser fixture; never requested
        "https:///SYNTHETIC_VALUE",
        "https://127.0.0.1\\SYNTHETIC_VALUE",
        "https://127.0.0.1/SYNTHETIC_VALUE value",
        "https://127.0.0.1:/pack.tar.gz",
        "//127.0.0.1/pack.tar.gz",
        "SYNTHETIC_VALUE",
    ] {
        let input = configured(&download(url, &"a".repeat(64)));
        let error = BridgeConfig::from_toml_str(&input).unwrap_err();
        assert_eq!(
            error.to_string(),
            "screen.packs[0].url: expected HTTPS URL without credentials or fragment"
        );
        assert!(!format!("{error:?}").contains("SYNTHETIC_VALUE"));
    }
}

#[test]
fn screen_pack_digest_and_entry_shapes_are_strict() {
    for digest in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "g".repeat(64),
        "é".repeat(32),
    ] {
        let error = BridgeConfig::from_toml_str(&configured(&download("https://127.0.0.1/a", &digest))).unwrap_err();
        assert_eq!(
            error.to_string(),
            "screen.packs[0].sha256: expected 64 hexadecimal digits"
        );
    }
    for packs in [
        "{}",
        "'SYNTHETIC_VALUE'",
        "[false]",
        "[{}]",
        "[{url='https://127.0.0.1/a'}]",
        "[{url='https://127.0.0.1/a',sha256=1}]",
        "[{url='https://127.0.0.1/a',sha256='a',SYNTHETIC_VALUE=true}]",
    ] {
        let error = BridgeConfig::from_toml_str(&configured(packs)).unwrap_err();
        assert_eq!(error.to_string(), "screen.packs: invalid type or value");
        assert!(!format!("{error:?}").contains("SYNTHETIC_VALUE"));
    }
}

#[test]
fn screen_pack_overlay_distinguishes_omitted_disabled_bare_and_versioned_selection() {
    let base = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    assert_eq!(SettingsOverlay::default().screen_pack(), None);
    for name in ["", "template", "local-pack", "template@0.1.0", "template@1.2.3-rc.1"] {
        let text = format!("screen_pack='{name}'");
        let overlay = SettingsOverlay::from_toml_str(&text).unwrap();
        assert_eq!(overlay.screen_pack(), Some(name));
        assert_eq!(overlay.apply_to(&base).unwrap(), base);
        let encoded = overlay.to_toml_string().unwrap();
        assert_eq!(SettingsOverlay::from_toml_str(&encoded).unwrap(), overlay);
    }
    let longest = "a".repeat(48);
    assert!(SettingsOverlay::from_toml_str(&format!("screen_pack='{longest}'")).is_ok());
}

#[test]
fn screen_pack_overlay_refuses_paths_invalid_versions_and_unknown_fields() {
    for value in [
        ".",
        "..",
        "../template",
        "/template",
        "C:/template",
        "a\\b",
        "Template",
        "a_b",
        "template@",
        "template@1.2",
        "template@1.2.3.4",
        "template@1.2.3-",
        "template@1.2.3-rc..1",
        "template@1.2.3+build",
        "template@1.2.3@other",
        "template@1.2.x",
        "template@1.2.3-rc-1",
    ] {
        let error = SettingsOverlay::from_toml_str(&format!("screen_pack='{value}'")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "screen_pack: expected pack name or name@version, or empty to disable"
        );
    }
    let long = "a".repeat(49);
    assert!(SettingsOverlay::from_toml_str(&format!("screen_pack='{long}'")).is_err());
    for value in ["true", "1", "[]", "{}"] {
        let error = SettingsOverlay::from_toml_str(&format!("screen_pack={value}")).unwrap_err();
        assert_eq!(error.to_string(), "screen_pack: invalid type or value");
    }
    assert_eq!(
        SettingsOverlay::from_toml_str("screen_pack='template'\nSYNTHETIC_VALUE=1")
            .unwrap_err()
            .to_string(),
        "config: unknown field"
    );
}

#[test]
fn screen_pack_sparse_merge_retains_voice_siblings_and_explicit_disable() {
    let initial = SettingsOverlay::from_toml_str("screen_pack='template@1.0.0'\n[agent]\nmax_tool_calls=4").unwrap();
    let voice = SettingsOverlay::from_leaves(Some(false), None, None, None, None);
    let merged = initial.merge(&voice);
    assert_eq!(merged.screen_pack(), Some("template@1.0.0"));
    let base = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    let effective = merged.apply_to(&base).unwrap();
    assert!(!effective.voice.session_listen_enabled);
    assert_eq!(effective.agent.max_tool_calls, 4);
    let disabled = merged.merge(&SettingsOverlay::from_toml_str("screen_pack=''").unwrap());
    assert_eq!(disabled.screen_pack(), Some(""));
    assert_eq!(disabled.apply_to(&base).unwrap(), effective);
    assert_eq!(
        SettingsOverlay::from_toml_str(&disabled.to_toml_string().unwrap()).unwrap(),
        disabled
    );
    assert_eq!(disabled.merge(&SettingsOverlay::default()), disabled);
}

#[test]
fn screen_pack_effective_load_keeps_selection_and_rejects_bad_overlay_wholly() {
    let directory = Directory::new().unwrap();
    let input = format!("{MINIMAL}\n[home]\ndir='{}'", directory.0.display());
    let config_path = directory.write(&input).unwrap();
    let settings_path = directory.0.join("settings.toml");
    std::fs::write(
        &settings_path,
        "screen_pack='template@0.1.0'\n[agent]\nmax_tool_calls=4",
    )
    .unwrap();
    let effective = BridgeConfig::load_effective(&config_path).unwrap();
    assert_eq!(effective.overlay.screen_pack(), Some("template@0.1.0"));
    assert_eq!(effective.config.agent.max_tool_calls, 4);
    assert!(effective.warning.is_none());
    let text = "screen_pack='../SYNTHETIC_VALUE'\n[agent]\nmax_tool_calls=2";
    std::fs::write(&settings_path, text).unwrap();
    let effective = BridgeConfig::load_effective(&config_path).unwrap();
    assert_eq!(effective.overlay.screen_pack(), None);
    assert_eq!(effective.config.agent.max_tool_calls, 8);
    assert_eq!(effective.config, effective.base);
    let error = effective.warning.unwrap();
    assert!(!format!("{error:?}").contains("SYNTHETIC_VALUE"));
    assert_eq!(std::fs::read_to_string(settings_path).unwrap(), text);
}
