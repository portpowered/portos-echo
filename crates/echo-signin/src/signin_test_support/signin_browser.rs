use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

struct Prompt {
    bytes: Arc<Mutex<Vec<u8>>>,
    ready: Option<tokio::sync::oneshot::Sender<String>>,
}
impl Write for Prompt {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(ready) = self.ready.take() {
            ready
                .send(String::from_utf8(self.bytes.lock().unwrap().clone()).unwrap())
                .unwrap();
        }
        Ok(())
    }
}
fn fields(query: &str) -> BTreeMap<String, String> {
    query
        .split('&')
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap();
            let mut decoded = Vec::new();
            let mut bytes = value.bytes();
            while let Some(byte) = bytes.next() {
                decoded.push(match byte {
                    b'%' => u8::from_str_radix(
                        &String::from_utf8(vec![bytes.next().unwrap(), bytes.next().unwrap()]).unwrap(),
                        16,
                    )
                    .unwrap(),
                    b'+' => b' ',
                    other => other,
                });
            }
            (key.into(), String::from_utf8(decoded).unwrap())
        })
        .collect()
}
fn authorization(prompt: &str, mock: &Mock) -> BTreeMap<String, String> {
    let url = prompt.strip_prefix("url: ").unwrap().strip_suffix('\n').unwrap();
    let (endpoint, query) = url.split_once('?').unwrap();
    assert_eq!(endpoint, mock.config.authorization_endpoint);
    let parameters = fields(query);
    assert_eq!(parameters["response_type"], "code");
    assert_eq!(parameters["client_id"], "mock-client");
    assert_eq!(parameters["scope"], "mock scopes");
    assert_eq!(parameters["code_challenge_method"], "S256");
    assert_eq!(parameters["state"].len(), 43);
    assert_eq!(parameters["code_challenge"].len(), 43);
    assert_eq!(parameters.len(), 10);
    let redirect = parameters["redirect_uri"].replace(':', "%3A").replace('/', "%2F");
    assert_eq!(
        url,
        format!(
            "{endpoint}?response_type=code&client_id=mock-client&redirect_uri={redirect}&scope=mock+scopes&code_challenge={}&code_challenge_method=S256&state={}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=echo-bridge",
            parameters["code_challenge"], parameters["state"]
        )
    );
    parameters
}

#[tokio::test]
async fn f07_browser_exchange_timeout_and_drop_preserve_storage() {
    for cancel in [false, true] {
        let mut mock = Mock::start(vec![(1, String::new())]).await;
        mock.config.browser_redirect_uri = "http://127.0.0.1:0/auth/callback".into();
        let directory = Directory::new().unwrap();
        let live = directory.live(LiveMode::Chatgpt);
        fs::write(&live.credential_file, b"previous bytes").unwrap();
        let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
        let (ready, receive) = tokio::sync::oneshot::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut output = Prompt {
            bytes: bytes.clone(),
            ready: Some(ready),
        };
        let mut operation = Box::pin(providers.execute(OperatorCommand::Signin { browser: true }, &mut output));
        let browser = async {
            let prompt = receive.await.unwrap();
            let parameters = authorization(&prompt, &mock);
            assert_eq!(callback(&parameters["redirect_uri"], &parameters["state"]).await, 200);
            (prompt, parameters)
        };
        let (prompt, _parameters) = tokio::select! {
            result = &mut operation => panic!("completed before exchange: {result:?}"),
            result = browser => result,
        };
        tokio::select! {
            result = &mut operation => panic!("stalled exchange completed: {result:?}"),
            seen = mock.seen.recv() => assert_eq!(seen, Some(0)),
        }
        let listener = listener_witness(&providers);
        if cancel {
            drop(operation);
        } else {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
            assert_eq!(operation.await, Err(SigninError::Auth(AuthError::Timeout)));
            tokio::time::resume();
        }
        assert_eq!(*bytes.lock().unwrap(), prompt.as_bytes());
        assert_eq!(fs::read(&live.credential_file).unwrap(), b"previous bytes");
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
        assert!(listener.upgrade().is_none());
        assert_eq!(
            providers.execute(OperatorCommand::Status, &mut Vec::new()).await,
            Err(SigninError::Auth(AuthError::InvalidCredential))
        );
        mock.stop().await;
    }
    writeln!(
        std::io::stdout(),
        "F07 exchange: fake-clock timeout/drop; old bytes preserved; listeners and locks released; exchanges=1"
    )
    .unwrap();
}
async fn callback(redirect: &str, state: &str) -> u16 {
    let address = redirect.strip_prefix("http://").unwrap().split('/').next().unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!("GET /auth/callback?state={state}&code=synthetic-code HTTP/1.1\r\nHost: localhost\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    String::from_utf8(bytes)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}
fn listener_witness(providers: &crate::Providers) -> std::sync::Weak<tokio::net::TcpListener> {
    let listener = providers.state.auth.as_ref().unwrap().browser_listener_witness();
    assert_eq!(
        listener.upgrade().unwrap().local_addr().unwrap().ip(),
        std::net::Ipv4Addr::LOCALHOST
    );
    listener
}

#[tokio::test]
async fn f06_browser_callback_commits_then_reports_success() {
    let mut mock = Mock::start(vec![approved().pop().unwrap()]).await;
    mock.config.browser_redirect_uri = "http://127.0.0.1:0/auth/callback".into();
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::Chatgpt);
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    let (ready, receive) = tokio::sync::oneshot::channel();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let mut output = Prompt {
        bytes: bytes.clone(),
        ready: Some(ready),
    };
    let operation = providers.execute(OperatorCommand::Signin { browser: true }, &mut output);
    let browser = async {
        let prompt = receive.await.unwrap();
        let parameters = authorization(&prompt, &mock);
        let listener = listener_witness(&providers);
        assert_eq!(callback(&parameters["redirect_uri"], &parameters["state"]).await, 200);
        (prompt, parameters, listener)
    };
    let (result, (prompt, parameters, listener)) = tokio::join!(operation, browser);
    assert_eq!(result, Ok(()));
    assert_eq!(*bytes.lock().unwrap(), format!("{prompt}signed in\n").as_bytes());
    let source = AuthSource::new(mock.config.clone(), live.credential_file).unwrap();
    let stored = source.current().await.unwrap();
    assert_eq!(stored.access_token.expose(), &token());
    assert_eq!(stored.account_id, ACCOUNT);
    {
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/token");
        let form = fields(&requests[0].body);
        assert_eq!(form["code"], "synthetic-code");
        assert_eq!(form["redirect_uri"], parameters["redirect_uri"]);
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["client_id"], "mock-client");
        assert_eq!(form["code_verifier"].len(), 43);
        assert_eq!(form.len(), 5);
    }
    no_secrets(&bytes.lock().unwrap());
    assert!(listener.upgrade().is_none());
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F06: exact URL/output; exchange=1; credential reload matches; listener released"
    )
    .unwrap();
}

#[tokio::test]
async fn f07_browser_wrong_state_timeout_and_drop_preserve_storage() {
    for action in ["wrong-state", "timeout", "drop"] {
        let mut mock = Mock::start(vec![]).await;
        mock.config.browser_redirect_uri = "http://127.0.0.1:0/auth/callback".into();
        let directory = Directory::new().unwrap();
        let live = directory.live(LiveMode::Chatgpt);
        fs::write(&live.credential_file, b"previous bytes").unwrap();
        let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
        let (ready, mut receive) = tokio::sync::oneshot::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut output = Prompt {
            bytes: bytes.clone(),
            ready: Some(ready),
        };
        let mut operation = Box::pin(providers.execute(OperatorCommand::Signin { browser: true }, &mut output));
        let prompt = tokio::select! {
            result = &mut operation => panic!("completed before callback: {result:?}"),
            prompt = &mut receive => prompt.unwrap(),
        };
        let parameters = authorization(&prompt, &mock);
        let listener = listener_witness(&providers);
        let retained = listener.upgrade().unwrap();
        match action {
            "wrong-state" => {
                let (result, status) = tokio::join!(operation, callback(&parameters["redirect_uri"], "wrong"));
                assert_eq!(status, 400);
                assert_eq!(result, Err(SigninError::Auth(AuthError::StateMismatch)));
            }
            "timeout" => {
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(901)).await;
                assert_eq!(operation.await, Err(SigninError::Auth(AuthError::Timeout)));
                tokio::time::resume();
            }
            _ => drop(operation),
        }
        assert_eq!(*bytes.lock().unwrap(), prompt.as_bytes());
        assert_eq!(fs::read(&live.credential_file).unwrap(), b"previous bytes");
        assert_eq!(mock.requests.lock().unwrap().len(), 0);
        assert!(
            listener.upgrade().is_some(),
            "retained real listener prevents release proof"
        );
        drop(retained);
        assert!(listener.upgrade().is_none());
        assert_eq!(
            providers.execute(OperatorCommand::Status, &mut Vec::new()).await,
            Err(SigninError::Auth(AuthError::InvalidCredential))
        );
        mock.stop().await;
    }
    writeln!(
        std::io::stdout(),
        "F07: state mismatch/fake-clock timeout/drop; old bytes preserved; listeners and locks released; exchanges=0"
    )
    .unwrap();
}

#[tokio::test]
async fn browser_prompt_output_failure_releases_listener_without_exchange() {
    let mut mock = Mock::start(vec![]).await;
    mock.config.browser_redirect_uri = "http://127.0.0.1:0/auth/callback".into();
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::Chatgpt);
    fs::write(&live.credential_file, b"previous bytes").unwrap();
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    let mut output = Writer::new(&mock, 0);
    assert_eq!(
        providers
            .execute(OperatorCommand::Signin { browser: true }, &mut output)
            .await,
        Err(SigninError::Output)
    );
    let listener = providers.state.auth.as_ref().unwrap().browser_listener_witness();
    assert!(
        !listener.ptr_eq(&std::sync::Weak::new()),
        "callback listener was acquired"
    );
    assert!(listener.upgrade().is_none());
    assert_eq!(output.bytes, b"");
    assert_eq!(fs::read(&live.credential_file).unwrap(), b"previous bytes");
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    assert_eq!(*mock.events.lock().unwrap(), Vec::<String>::new());
    mock.stop().await;
}
