use super::*;

#[test]
fn muse_origin_refuses_credentials_paths_and_nonlocal_lab_hosts() {
    for text in ["https://127.0.0.1:7448", "https://127.0.0.1:7448/"] {
        let url = origin(text, true).unwrap();
        assert_eq!(url.as_str(), "https://127.0.0.1:7448/");
        assert_eq!(
            url.join("/v1/tasks").unwrap().as_str(),
            "https://127.0.0.1:7448/v1/tasks"
        );
    }
    assert_eq!(
        origin("https://muse.example", false).unwrap().host_str(),
        Some("muse.example")
    );
    // lint-rules: allow-literal URL parser rejection fixtures, never contacted.
    for text in [
        "http://127.0.0.1:7448",
        "https://user@127.0.0.1:7448",
        "https://127.0.0.1:0",
        "https://127.0.0.1:7448/path",
        "https://127.0.0.1:7448/?x",
        "https://127.0.0.1:7448/#x",
        "https://localhost:7448",
        "https://[::1]:7448",
        "https://muse.example",
        "https://127.1:7448",
        "https://@127.0.0.1:7448",
        "https://127.0.0.1:7448\\path",
    ] {
        assert_eq!(origin(text, true).unwrap_err(), Error::Input);
    }
}

#[test]
fn muse_credential_accepts_only_daemon_compatible_bytes_and_hides_header() {
    for size in [32, 256] {
        for suffix in ["", "\n"] {
            let input = format!("{}{suffix}", "x".repeat(size));
            let value = credential(input.into_bytes()).unwrap();
            assert_eq!(
                value.expose().as_bytes(),
                format!("Bearer {}", "x".repeat(size)).as_bytes()
            );
            assert!(value.expose().is_sensitive());
            assert_eq!(format!("{:?}", value.expose()), "Sensitive");
        }
    }
    for input in [
        vec![],
        vec![b'x'; 31],
        vec![b'x'; 257],
        vec![b' '; 32],
        vec![0xff; 32],
        format!("{}\r\n", "x".repeat(32)).into_bytes(),
        format!("{}\n\n", "x".repeat(32)).into_bytes(),
    ] {
        assert_eq!(credential(input).err(), Some(Error::Input));
    }
}

#[test]
fn muse_private_file_reads_are_bounded_and_errors_do_not_contain_paths() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private");
    assert_eq!(read(&path, 32), Err(Error::Input));
    std::fs::write(&path, [b'x'; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(read(&path, 32).unwrap(), [b'x'; 32]);
    assert_eq!(read(&path, 31), Err(Error::Input));
    assert_eq!(read(directory.path(), 32), Err(Error::Input));
    assert_eq!(Error::Input.to_string(), "invalid input");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read(&path, 32), Err(Error::Input));
    }
}

#[test]
fn muse_invalid_der_trust_fails_offline_with_a_fixed_diagnostic() {
    let directory = tempfile::tempdir().unwrap();
    let config = MuseConfig {
        url: "https://127.0.0.1:7448".into(),
        ca_file: directory.path().join("ca.der"),
        credential_file: directory.path().join("credential"),
    };
    std::fs::write(&config.ca_file, b"invalid certificate").unwrap();
    std::fs::write(&config.credential_file, [b'x'; 32]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&config.ca_file, &config.credential_file] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    assert_eq!(Http::open(&config, true).err(), Some(Error::Input));
}
