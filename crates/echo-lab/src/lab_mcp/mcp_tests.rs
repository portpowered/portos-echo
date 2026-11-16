//! T3: the real auth client/store and spoof HTTP boundary, local counterparts.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use echo_mcp_auth::{AuthClient, AuthOptions, TokenProvider};
use echo_spoof_mcp::{Options, Spoof};
use p256::ecdsa::{Signature, signature::Signer};
use serde_json::json;
use std::{
    future::Future,
    task::Poll,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

async fn drive<F: Future>(future: F) -> F::Output {
    tokio::pin!(future);
    std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
        Poll::Ready(value) => Poll::Ready(value),
        Poll::Pending => {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

struct Harness {
    fixture: Fixture,
    spoof: Spoof,
    dir: tempfile::TempDir,
    client: Arc<AuthClient>,
}

impl Harness {
    fn options(&self) -> AuthOptions {
        AuthOptions {
            resource_url: self.spoof.url().into(),
            client_id: "echo-lab".into(),
            scopes: vec!["endpoint:read".into()],
            storage_dir: self.dir.path().into(),
            request_timeout: Duration::from_secs(2),
        }
    }

    async fn new() -> Self {
        let fixture = Fixture::bind(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs())
            .await
            .unwrap();
        let spoof = Spoof::start_authorized(Options::default(), Some(fixture.validator()))
            .await
            .unwrap();
        fixture.set_resource(spoof.url()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let options = AuthOptions {
            resource_url: spoof.url().into(),
            client_id: "echo-lab".into(),
            scopes: vec!["endpoint:read".into()],
            storage_dir: dir.path().into(),
            request_timeout: Duration::from_secs(2),
        };
        let client = Arc::new(drive(AuthClient::open(options)).await.unwrap());
        Self {
            fixture,
            spoof,
            dir,
            client,
        }
    }

    fn record(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.path().join("grant.json")).unwrap()).unwrap()
    }

    async fn link(&self) {
        let task = self.start_link().await;
        self.fixture.advance(5).unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        drive(task).await.unwrap().unwrap();
    }

    async fn start_link(&self) -> JoinHandle<Result<(), echo_mcp_auth::AuthError>> {
        let link = drive(self.client.begin_link()).await.unwrap();
        assert_eq!(link.user_code, "TEST-CODE");
        assert_eq!(link.verification_uri, format!("{}/approve", self.fixture.base()));
        assert_eq!((link.expires_in, link.interval), (1800, 5));
        self.fixture.approve(Approval::Approved).unwrap();
        let client = self.client.clone();
        let task = tokio::spawn(async move { client.await_approval(link).await });
        tokio::task::yield_now().await;
        task
    }

    async fn request(&self, access: &str, method: &str) -> reqwest::Response {
        let params = match method {
            "initialize" => json!({"protocolVersion":"2025-03-26","capabilities":{},
                "clientInfo":{"name":"echo-lab","version":"0.1.0"}}),
            "tools/list" => json!({}),
            _ => json!({"name":"test.echo","arguments":{"text":"m5-linked"}}),
        };
        drive(
            http()
                .post(self.spoof.url())
                .bearer_auth(access)
                .json(&json!({"jsonrpc":"2.0","id":1,
            "method":method,"params":params}))
                .send(),
        )
        .await
        .unwrap()
    }

    async fn close(self) {
        let url = self.spoof.url().to_owned();
        let base = self.fixture.base().to_owned();
        drive(self.spoof.shutdown()).await.unwrap();
        drive(self.fixture.shutdown()).await.unwrap();
        assert!(drive(http().post(url).send()).await.is_err());
        assert!(drive(http().get(base).send()).await.is_err());
    }
}

fn proof(record: &Value, url: &str, now: u64, id: &str) -> String {
    let jwk = &record["private_key_jwk"];
    let key = SigningKey::from_slice(&B64.decode(jwk["d"].as_str().unwrap()).unwrap()).unwrap();
    let header = json!({"typ":"dpop+jwt","alg":"ES256","jwk":{"kty":"EC","crv":"P-256","x":jwk["x"],"y":jwk["y"]}});
    let claims = json!({"htm":"POST","htu":url,"iat":now,"jti":id});
    let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
    let signature: Signature = key.sign(input.as_bytes());
    format!("{input}.{}", B64.encode(signature.to_bytes()))
}

#[tokio::test(start_paused = true)]
async fn t3_m5_strict_link_rotate_reload_and_replay_revoke_resource_access() {
    let h = Harness::new().await;
    h.link().await;
    let initial = h.record();
    for generation in 2..=3 {
        let access = drive(h.client.force_refresh()).await.unwrap();
        let stored = h.record();
        assert_eq!(stored["grant"]["access_token"], *access.expose());
        assert_eq!(
            stored["grant"]["refresh_token"],
            format!("synthetic-refresh-1-{generation}")
        );
        assert_eq!(stored["private_key_jwk"], initial["private_key_jwk"]);
    }
    let reloaded = drive(AuthClient::open(h.options())).await.unwrap();
    let token = drive(reloaded.access_token()).await.unwrap();
    assert_eq!(
        h.fixture.counts().unwrap(),
        Counts {
            links: 1,
            polls: 1,
            refreshes: 2,
            ..Counts::default()
        }
    );
    assert_eq!(h.request(token.expose(), "tools/list").await.status(), 200);
    let result: Value = drive(h.request(token.expose(), "tools/call").await.json())
        .await
        .unwrap();
    assert_eq!(result["result"]["content"], json!([{"type":"text","text":"m5-linked"}]));
    let url = format!("{}/custom/token", h.fixture.base());
    let now = h.fixture.shared.lock().unwrap().now;
    let response = drive(
        http()
            .post(&url)
            .header("DPoP", proof(&initial, &url, now, "replay"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", "echo-lab"),
                ("refresh_token", initial["grant"]["refresh_token"].as_str().unwrap()),
            ])
            .send(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        drive(response.json::<Value>()).await.unwrap(),
        json!({"error":"invalid_grant"})
    );
    for method in ["initialize", "tools/list", "tools/call"] {
        assert_eq!(h.request(token.expose(), method).await.status(), 401);
    }
    assert_eq!(h.fixture.counts().unwrap().replay_rejected, 1);
    let snapshot = h.spoof.snapshot().unwrap();
    assert_eq!(snapshot.calls.len(), 1);
    assert_eq!(snapshot.calls[0].args, json!({"text":"m5-linked"}));
    assert!(snapshot.requests.iter().all(|r| !r.dpop_header));
    h.close().await;
}

#[tokio::test(start_paused = true)]
async fn t3_m5_signed_wrong_claims_forgery_dpop_and_revocation_never_dispatch() {
    let h = Harness::new().await;
    h.link().await;
    let token = drive(h.client.access_token()).await.unwrap();
    let (_, original, _, _) = mcp_proof::decode(token.expose()).unwrap();
    for (field, value) in [
        ("iss", json!("wrong")),
        ("aud", json!("wrong")),
        ("sub", json!("")),
        ("token_use", json!("refresh")),
        ("grant_id", json!("wrong")),
        ("exp", original["iat"].clone()),
        ("iat", json!(u64::MAX)),
        ("scope", json!("other")),
    ] {
        let mut claims = original.clone();
        claims[field] = value;
        let bad = mcp_tokens::sign(&h.fixture.shared.lock().unwrap(), &claims);
        for method in ["initialize", "tools/list", "tools/call"] {
            assert_eq!(h.request(&bad, method).await.status(), 401, "{field} {method}");
        }
    }
    let (header, claims, _, _) = mcp_proof::decode(token.expose()).unwrap();
    let forged = format!(
        "{}.{}.{}",
        B64.encode(header.to_string()),
        B64.encode(claims.to_string()),
        B64.encode([0_u8; 64])
    );
    assert_eq!(h.request(&forged, "tools/call").await.status(), 401);
    assert_eq!(h.request("malformed", "tools/call").await.status(), 401);
    let response = drive(
        http()
            .post(h.spoof.url())
            .bearer_auth(token.expose())
            .header("DPoP", "unwanted")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .send(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 401);
    drive(h.client.revoke()).await.unwrap();
    drive(h.client.revoke()).await.unwrap();
    assert_eq!(h.fixture.counts().unwrap().revokes, 1);
    assert_eq!(h.request(token.expose(), "tools/call").await.status(), 401);
    assert_eq!(h.record()["grant"], Value::Null);
    assert_eq!(h.spoof.snapshot().unwrap().calls, vec![]);
    h.close().await;
}

#[tokio::test(start_paused = true)]
async fn t3_m5_wrong_resource_pending_denied_and_unbound_tokens() {
    let h = Harness::new().await;
    let record = h.record();
    let base = h.fixture.base();
    let now = h.fixture.shared.lock().unwrap().now;
    let url = format!("{base}/custom/link");
    let send = |resource: Option<&str>, id: &str| {
        let mut form = vec![("client_id", "echo-lab"), ("scope", "endpoint:read")];
        if let Some(resource) = resource {
            form.push(("resource", resource));
        }
        http()
            .post(&url)
            .header("DPoP", proof(&record, &url, now, id))
            .form(&form)
            .send()
    };
    let response = drive(send(Some("wrong"), "wrong-resource")).await.unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        drive(response.json::<Value>()).await.unwrap(),
        json!({"error":"invalid_target"})
    );
    assert_eq!(h.fixture.counts().unwrap().links, 0);
    assert_eq!(drive(send(None, "unbound")).await.unwrap().status(), 200);
    let token_url = format!("{base}/custom/token");
    let poll = |id: &str, time| {
        http()
            .post(&token_url)
            .header("DPoP", proof(&record, &token_url, time, id))
            .form(&[
                ("client_id", "echo-lab"),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", "synthetic-device-1"),
            ])
            .send()
    };
    let response = drive(poll("pending", now)).await.unwrap();
    assert_eq!(
        drive(response.json::<Value>()).await.unwrap(),
        json!({"error":"authorization_pending"})
    );
    h.fixture.approve(Approval::Denied).unwrap();
    let response = drive(poll("denied", now)).await.unwrap();
    assert_eq!(
        drive(response.json::<Value>()).await.unwrap(),
        json!({"error":"access_denied"})
    );
    h.fixture.approve(Approval::Approved).unwrap();
    let response = drive(poll("approved", now)).await.unwrap();
    assert_eq!(response.status(), 200);
    let pair: Value = drive(response.json()).await.unwrap();
    assert_eq!(
        h.request(pair["access_token"].as_str().unwrap(), "tools/call")
            .await
            .status(),
        401
    );
    assert_eq!(h.spoof.snapshot().unwrap().calls, vec![]);
    h.close().await;
}

#[tokio::test(start_paused = true)]
async fn t3_m5_proofs_are_exact_single_use_and_code_expiry_is_inclusive() {
    let h = Harness::new().await;
    let record = h.record();
    let url = format!("{}/custom/link", h.fixture.base());
    let now = h.fixture.shared.lock().unwrap().now;
    let form = [
        ("client_id", "echo-lab"),
        ("scope", "endpoint:read"),
        ("resource", h.spoof.url()),
    ];
    let send = |value: String| http().post(&url).header("DPoP", value).form(&form).send();
    for value in [
        "invalid".into(),
        proof(&record, &format!("{url}/wrong"), now, "wrong-path"),
        proof(&record, &url, now - 30, "stale"),
    ] {
        let response = drive(send(value)).await.unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            drive(response.json::<Value>()).await.unwrap(),
            json!({"error":"invalid_dpop_proof"})
        );
    }
    let value = proof(&record, &url, now, "once");
    assert_eq!(drive(send(value.clone())).await.unwrap().status(), 200);
    let response = drive(send(value)).await.unwrap();
    assert_eq!(
        drive(response.json::<Value>()).await.unwrap(),
        json!({"error":"invalid_dpop_proof"})
    );
    assert_eq!(h.fixture.counts().unwrap().links, 1);
    h.fixture.approve(Approval::Approved).unwrap();
    h.fixture.advance(1800).unwrap();
    let url = format!("{}/custom/token", h.fixture.base());
    let response = drive(
        http()
            .post(&url)
            .header("DPoP", proof(&record, &url, now + 1800, "expired"))
            .form(&[
                ("client_id", "echo-lab"),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", "synthetic-device-1"),
            ])
            .send(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        drive(response.json::<Value>()).await.unwrap(),
        json!({"error":"expired_token"})
    );
    assert_eq!(h.record()["grant"], Value::Null);
    h.close().await;
}

#[tokio::test(start_paused = true)]
async fn t3_m5_parallel_sessions_keep_grants_and_cleanup_independent() {
    let (a, b) = tokio::join!(Harness::new(), Harness::new());
    let (pending_a, pending_b) = tokio::join!(a.start_link(), b.start_link());
    a.fixture.advance(5).unwrap();
    b.fixture.advance(5).unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    let (a_linked, b_linked) = drive(async { tokio::join!(pending_a, pending_b) }).await;
    a_linked.unwrap().unwrap();
    b_linked.unwrap().unwrap();
    let token_a = drive(a.client.access_token()).await.unwrap();
    let token_b = drive(b.client.access_token()).await.unwrap();
    assert_eq!(b.request(token_a.expose(), "tools/call").await.status(), 401);
    assert_eq!(a.request(token_b.expose(), "tools/call").await.status(), 401);
    a.close().await;
    assert_eq!(b.request(token_b.expose(), "tools/call").await.status(), 200);
    assert_eq!(b.spoof.snapshot().unwrap().calls.len(), 1);
    assert_eq!(b.fixture.counts().unwrap().links, 1);
    b.close().await;
}
