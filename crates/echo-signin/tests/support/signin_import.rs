use super::*;

fn imported(account: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "tokens": {"access_token":token(), "refresh_token":REFRESH, "account_id":account},
        "last_refresh":"2096-01-01T00:00:00Z"
    }))
    .unwrap()
}

#[tokio::test]
async fn import_writer_failure_keeps_committed_copy_and_lock_contention_is_safe() {
    let mock = Mock::start(vec![]).await;
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::Chatgpt);
    let source = directory.0.join("explicit-source");
    let original = imported(ACCOUNT);
    fs::write(&source, &original).unwrap();
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.0.join("credential.lock"))
        .unwrap();
    lock.lock().unwrap();
    let mut output = Vec::new();
    assert_eq!(
        providers
            .execute(OperatorCommand::ImportCodex { path: source.clone() }, &mut output)
            .await,
        Err(SigninError::Auth(AuthError::Busy))
    );
    assert_eq!(output, b"");
    assert!(!live.credential_file.exists());
    lock.unlock().unwrap();
    drop(lock);
    let mut output = Writer::new(&mock, 0);
    assert_eq!(
        providers
            .execute(OperatorCommand::ImportCodex { path: source.clone() }, &mut output)
            .await,
        Err(SigninError::Output)
    );
    assert_eq!(output.bytes, b"");
    let credential = AuthSource::new(mock.config.clone(), live.credential_file)
        .unwrap()
        .current()
        .await
        .unwrap();
    assert_eq!(credential.access_token.expose(), &token());
    assert_eq!(credential.account_id, ACCOUNT);
    assert_eq!(fs::read(source).unwrap(), original);
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    mock.stop().await;
}

#[tokio::test]
async fn f08_f09_explicit_import_owns_copy_and_refuses_reimport() {
    let mock = Mock::start(vec![]).await;
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::Chatgpt);
    let source = directory.0.join("explicit-source");
    let original = imported(ACCOUNT);
    fs::write(&source, &original).unwrap();
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    let mut output = Vec::new();
    providers
        .execute(OperatorCommand::ImportCodex { path: source.clone() }, &mut output)
        .await
        .unwrap();
    assert_eq!(output, b"imported\n");
    let stored = fs::read(&live.credential_file).unwrap();
    let auth = AuthSource::new(mock.config.clone(), live.credential_file.clone()).unwrap();
    let credential = auth.current().await.unwrap();
    assert_eq!(credential.access_token.expose(), &token());
    assert_eq!(credential.account_id, ACCOUNT);
    output.clear();
    providers.execute(OperatorCommand::Status, &mut output).await.unwrap();
    assert_eq!(output, b"mode: chatgpt\naccount: ****1234\nexpires: 4000000000\n");
    no_secrets(&output);
    output.clear();
    assert_eq!(
        providers
            .execute(OperatorCommand::ImportCodex { path: source.clone() }, &mut output)
            .await,
        Err(SigninError::Auth(AuthError::AlreadyImported))
    );
    assert_eq!(output, b"");
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(fs::read(&live.credential_file).unwrap(), stored);
    fs::remove_file(source).unwrap();
    assert_eq!(auth.status().await.unwrap().expires, 4_000_000_000);
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F08/F09: exact import/status; independent copy; second import refused; source unchanged; HTTP=0"
    )
    .unwrap();
}

#[tokio::test]
async fn f10_import_failures_leave_destination_absent() {
    let mock = Mock::start(vec![]).await;
    for (bytes, error) in [
        (None, AuthError::NotSignedIn),
        (Some(b"malformed".to_vec()), AuthError::InvalidCredential),
        (
            Some(imported("different-synthetic-account")),
            AuthError::AccountMismatch,
        ),
    ] {
        let directory = Directory::new().unwrap();
        let live = directory.live(LiveMode::Chatgpt);
        let source = directory.0.join("explicit-source");
        if let Some(bytes) = &bytes {
            fs::write(&source, bytes).unwrap();
        }
        let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            providers
                .execute(OperatorCommand::ImportCodex { path: source.clone() }, &mut output)
                .await,
            Err(SigninError::Auth(error))
        );
        assert_eq!(output, b"");
        assert!(!live.credential_file.exists());
        assert_eq!(fs::read(source).ok(), bytes);
        no_secrets(error.to_string().as_bytes());
    }
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F10: missing/invalid/mismatch exact; no destination or success output; HTTP=0"
    )
    .unwrap();
}

#[tokio::test]
async fn f17_api_key_rejects_all_setup_commands_without_io() {
    let mock = Mock::start(vec![]).await;
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::ApiKey);
    fs::write(&live.credential_file, b"synthetic-key-file").unwrap();
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    for command in [
        signin(),
        OperatorCommand::Signin { browser: true },
        OperatorCommand::ImportCodex {
            path: directory.0.join("absent"),
        },
    ] {
        let mut output = Vec::new();
        assert_eq!(
            providers.execute(command, &mut output).await,
            Err(SigninError::UnsupportedMode)
        );
        assert_eq!(output, b"");
        assert_eq!(fs::read(&live.credential_file).unwrap(), b"synthetic-key-file");
    }
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F17: device/browser/import unsupported; output=0; files unchanged; HTTP=0"
    )
    .unwrap();
}
