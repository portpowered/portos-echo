use std::{io, path::PathBuf, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt as _, StreamExt as _};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    net::{TcpListener, TcpStream},
    time::Instant,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Error, Message,
        handshake::server::{Request as WsRequest, Response as WsResponse},
        http::StatusCode,
    },
};
use x25519_dalek::StaticSecret;

use crate::{
    clock::{Clock as _, EpochClock},
    control,
    noise::{Session, responder::Responder},
    wire::{Assembler, Body, Frame, Packetizer, Request, Response, decode_request, encode_response},
};

use super::{pairing_record, write_pairing};

pub(super) struct Pki {
    pub(super) pem: Vec<u8>,
    issuer: Issuer<'static, KeyPair>,
}

impl Pki {
    pub(super) fn new() -> Self {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let key = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            STANDARD.encode(cert.der())
        )
        .into_bytes();
        Self {
            pem,
            issuer: Issuer::new(params, key),
        }
    }

    fn server(&self, dns: &str) -> TlsAcceptor {
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec![dns.to_owned()])
            .unwrap()
            .signed_by(&key, &self.issuer)
            .unwrap();
        let tls = ServerConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
        TlsAcceptor::from(Arc::new(tls))
    }
}

pub(super) struct Fake {
    _directory: tempfile::TempDir,
    pub(super) pairing: PathBuf,
    pub(super) api: TcpListener,
    pub(super) vm_listener: TcpListener,
    api_tls: TlsAcceptor,
    vm_tls: TlsAcceptor,
}

impl Fake {
    pub(super) fn task_path(&self) -> PathBuf {
        self._directory.path().join("tasks.json")
    }

    pub(super) async fn new(api: &Pki, vm: &Pki, api_name: &str) -> Self {
        Self::listeners(api.server(api_name), vm.server("127.0.0.1")).await
    }

    pub(super) async fn with_vm_name(api: &Pki, vm: &Pki, vm_name: &str) -> Self {
        Self::listeners(api.server("127.0.0.1"), vm.server(vm_name)).await
    }

    async fn listeners(api_tls: TlsAcceptor, vm_tls: TlsAcceptor) -> Self {
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let vm_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let pairing = directory.path().join("pairing.json");
        let mut record = pairing_record();
        record["access_token_saved_at"] = json!(EpochClock.now());
        record["noise_host"] = json!(vm_listener.local_addr().unwrap().to_string());
        let mut api_url = reqwest::Url::parse("https://127.0.0.1").unwrap();
        api_url.set_port(Some(api.local_addr().unwrap().port())).unwrap();
        record["api_url_v2"] = json!(api_url.as_str());
        write_pairing(&pairing, &record);
        Self {
            _directory: directory,
            pairing,
            api,
            vm_listener,
            api_tls,
            vm_tls,
        }
    }

    pub(super) async fn accept_api(&self) -> io::Result<TlsStream<TcpStream>> {
        self.api_tls.accept(self.api.accept().await.unwrap().0).await
    }

    pub(super) async fn accept_vm(&self) -> io::Result<TlsStream<TcpStream>> {
        self.vm_tls.accept(self.vm_listener.accept().await.unwrap().0).await
    }

    pub(super) async fn lookup(&self) {
        self.leases(
            "synthetic-access",
            json!([
                {"vm_ws_url": "wss://127.0.0.1:1", "vm_auth_token": "synthetic-other", "vm_id": "other"},
                {"vm_url": "wss://127.0.0.1:1", "vm_auth_token": "synthetic-default", "vm_id": "vm 1&x", "default": true}
            ]),
        )
        .await;
    }

    pub(super) async fn leases(&self, token: &str, leases: Value) {
        let (socket, headers) = self.api_request("GET /fetch_vms", token).await;
        assert!(
            headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("x-api-version: 1.0.0"))
        );
        api_reply(socket, 200, json!({"vm_list": leases})).await;
    }

    pub(super) async fn refresh(&self, status: u16, body: Value) {
        let socket = self.read_refresh().await;
        api_reply(socket, status, body).await;
    }

    async fn read_refresh(&self) -> BufReader<TlsStream<TcpStream>> {
        let (mut socket, headers) = self
            .api_request("POST /device_token/refresh", "hatch_refresh:synthetic-refresh")
            .await;
        let length = header(&headers, "content-length").parse::<usize>().unwrap();
        assert!((1..=4096).contains(&length));
        let mut data = vec![0; length];
        socket.read_exact(&mut data).await.unwrap();
        assert_eq!(header(&headers, "content-type"), "application/json");
        assert_eq!(
            serde_json::from_slice::<Value>(&data).unwrap(),
            json!({"device_id": "gadget-fixture"})
        );
        socket
    }

    async fn api_request(&self, target: &str, token: &str) -> (BufReader<TlsStream<TcpStream>>, String) {
        let mut socket = BufReader::new(self.accept_api().await.unwrap());
        let mut headers = String::new();
        loop {
            let previous = headers.len();
            assert!(socket.read_line(&mut headers).await.unwrap() > 0);
            assert!(headers.len() <= 4096);
            if &headers[previous..] == "\r\n" {
                break;
            }
        }
        assert_eq!(headers.lines().next(), Some(format!("{target} HTTP/1.1").as_str()));
        assert_eq!(header(&headers, "authorization"), format!("Bearer {token}"));
        assert!(header(&headers, "user-agent").starts_with("musegadget/"));
        (socket, headers)
    }

    pub(super) async fn vm(&self) -> Vm {
        self.upgrade("vm%201%26x", "synthetic-default", None).await.unwrap()
    }

    pub(super) async fn upgrade(&self, id: &str, token: &str, reject: Option<StatusCode>) -> Option<Vm> {
        let socket = self.accept_vm().await.unwrap();
        let ws = accept_hdr_async(socket, |request: &WsRequest, response: WsResponse| {
            assert_eq!(
                request.uri().path_and_query().unwrap().as_str(),
                format!("/v1/noise?vm_id={id}")
            );
            assert_eq!(request.headers()["authorization"], format!("Bearer {token}"));
            if let Some(status) = reject {
                let mut response = tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(None);
                *response.status_mut() = status;
                return Err(response);
            }
            Ok(response)
        })
        .await;
        match reject {
            Some(status) => {
                assert!(matches!(ws, Err(Error::Http(response)) if response.status() == status));
                None
            }
            None => Some(Vm::handshake(ws.unwrap()).await),
        }
    }
}

fn header<'a>(headers: &'a str, name: &str) -> &'a str {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.trim()))
        .unwrap()
}

async fn api_reply(mut socket: BufReader<TlsStream<TcpStream>>, status: u16, body: Value) {
    let body = body.to_string();
    let reply = format!(
        "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let socket = socket.get_mut();
    socket.write_all(reply.as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}

type Socket = WebSocketStream<TlsStream<TcpStream>>;

pub(super) struct Vm {
    socket: Socket,
    noise: Session,
    assembler: Assembler,
    packetizer: Packetizer,
}

impl Vm {
    async fn handshake(mut socket: Socket) -> Self {
        let first = binary(&mut socket).await;
        let (handshake, payload, second) = Responder::reply(
            &StaticSecret::from([9; 32]),
            StaticSecret::from([11; 32]),
            &[],
            &first,
            &[],
        )
        .unwrap();
        assert_eq!(payload, b"");
        socket.send(Message::binary(second)).await.unwrap();
        let (payload, noise) = handshake.finish(&binary(&mut socket).await).unwrap();
        assert_eq!(payload, b"");
        Self {
            socket,
            noise,
            assembler: Assembler::default(),
            packetizer: Packetizer::default(),
        }
    }

    pub(in crate::tests) async fn next_frame(&mut self) -> Frame {
        loop {
            let packet = binary(&mut self.socket).await;
            let chunk = self.noise.receive.decrypt(packet, &[]).unwrap();
            if let Some(bytes) = self.assembler.push(&chunk, Instant::now()).unwrap() {
                return decode_request(&bytes).unwrap();
            }
        }
    }

    pub(in crate::tests) async fn send(&mut self, frame: &Frame) {
        for chunk in self.packetizer.split(&encode_response(frame)).unwrap() {
            let data = self.noise.send.encrypt(&chunk, &[]).unwrap();
            self.socket.send(Message::binary(data)).await.unwrap();
        }
    }

    pub(super) async fn read_control_request(&mut self) -> i64 {
        let frame = self.next_frame().await;
        assert_eq!(frame.id, 1);
        assert_eq!(
            frame.body,
            Body::Request(Request {
                verb: "POST".into(),
                path: "/link-control".into(),
                ..Request::default()
            })
        );
        frame.id
    }

    pub(super) async fn control_response(&mut self, id: i64, status: i32) {
        self.send(&Frame {
            id,
            body: Body::Response(Response {
                status,
                end_body: status >= 400,
                ..Response::default()
            }),
        })
        .await;
    }

    pub(super) async fn read_register(&mut self, id: i64) -> Value {
        let frame = self.next_frame().await;
        assert_eq!(frame.id, id);
        let Body::Chunk { data, end_body: false } = frame.body else {
            panic!("invalid registration stream body");
        };
        let mut decoder = control::Decoder::default();
        let mut data = data.as_slice();
        let message = decoder.next(&mut data).unwrap().unwrap();
        assert!(data.is_empty());
        message
    }

    pub(super) async fn register_ack(&mut self, id: i64, request: &Value, ok: bool) {
        let data = control::encode(&json!({"type": "res", "id": request, "ok": ok})).unwrap();
        self.send(&Frame {
            id,
            body: Body::Chunk { data, end_body: false },
        })
        .await;
    }

    pub(super) async fn expect_closed(&mut self) {
        if let Some(Ok(message)) = self.socket.next().await {
            assert!(message.is_close());
        }
    }

    pub(super) async fn barrier(&mut self) {
        self.socket
            .send(Message::Ping(b"ack barrier".as_slice().into()))
            .await
            .unwrap();
        let reply = self.socket.next().await.unwrap().unwrap();
        assert_eq!(reply, Message::Pong(b"ack barrier".as_slice().into()));
    }
}

async fn binary(socket: &mut Socket) -> Vec<u8> {
    let message = socket.next().await.unwrap().unwrap();
    assert!(message.is_binary());
    message.into_data().to_vec()
}
