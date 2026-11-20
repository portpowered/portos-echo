//! T1/T2: existing frame contract and private projection, no sockets/processes.
use super::{Provider, events::Events, prompt};
use crate::{Failure, Result};
use echo_screen_contract::{ScreenFrame, UiState};
use serde_json::{Value, json};

fn wire(seq: u64, state: Value) -> Vec<u8> {
    let mut frame = json!({"seq":seq});
    frame["state"] = state;
    format!("data: {frame}\n\n").into_bytes()
}

fn code() -> Value {
    json!({"state":"link_code", "user_code":"TEST-LINK",
        "verification_uri":"https://auth.portoperatingsystem.com/device", "expires_at_unix":120}) // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
}

#[test]
fn existing_prompt_values_survive_every_chunk_boundary_and_crlf() -> Result<()> {
    let state = json!({"state":"ready", "message":
        "code: TEST-CODE\nurl: https://auth.openai.com/codex/device\n"}); // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
    let bytes = wire(1, state);
    for bytes in [
        bytes.clone(),
        String::from_utf8_lossy(&bytes).replace('\n', "\r\n").into_bytes(),
    ] {
        for split in 0..=bytes.len() {
            let mut events = Events::default();
            events.push(&bytes[..split])?;
            events.push(&bytes[split..])?;
            let public = serde_json::to_value(events.next(Provider::Chatgpt, 100)?.ok_or(Failure("absent"))?)
                .map_err(|_| Failure("fixture"))?;
            assert_eq!(
                public,
                json!({"type":"prompt", "provider":"chatgpt",
                "user_code":"TEST-CODE", "verification_uri":"https://auth.openai.com/codex/device"}) // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
            );
            assert!(events.next(Provider::Chatgpt, 100)?.is_none());
        }
    }
    Ok(())
}

#[test]
fn only_matching_provider_projects_and_code_expires_at_exact_boundary() -> Result<()> {
    let mut events = Events::default();
    events.push(&wire(1, code()))?;
    assert!(events.next(Provider::Chatgpt, 100)?.is_none());
    events.push(&wire(2, code()))?;
    let public = serde_json::to_value(events.next(Provider::Portos, 119)?.ok_or(Failure("absent"))?)
        .map_err(|_| Failure("fixture"))?;
    assert_eq!(public["user_code"], "TEST-LINK");
    assert_eq!(
        public["verification_uri"],
        "https://auth.portoperatingsystem.com/device" // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
    );
    events.push(&wire(3, code()))?;
    assert_eq!(
        events.next(Provider::Portos, 120).err(),
        Some(Failure("setup_code_expired"))
    );
    Ok(())
}

#[test]
fn arbitrary_messages_extra_private_fields_and_error_payload_never_escape() -> Result<()> {
    let mut events = Events::default();
    events.push(&wire(
        0,
        json!({"state":"ready", "message":"SYNTHETIC-TOKEN SYNTHETIC-ACCOUNT"}),
    ))?;
    assert!(events.next(Provider::Chatgpt, 0)?.is_none());
    let mut state = code();
    state["token"] = "SYNTHETIC-TOKEN".into();
    state["account_id"] = "SYNTHETIC-ACCOUNT".into();
    events.push(&wire(1, state))?;
    let public = serde_json::to_string(&events.next(Provider::Portos, 100)?.ok_or(Failure("absent"))?)
        .map_err(|_| Failure("fixture"))?;
    for marker in ["SYNTHETIC-TOKEN", "SYNTHETIC-ACCOUNT"] {
        assert!(!public.contains(marker));
    }
    events.push(&wire(
        2,
        json!({"state":"error", "message":"SYNTHETIC-TOKEN", "relink":true}),
    ))?;
    assert_eq!(
        events.next(Provider::Portos, 100).err(),
        Some(Failure("setup_provider_failed"))
    );
    Ok(())
}

#[test]
fn refuses_control_codes_url_credentials_queries_redirect_origins_and_prompt_trailers() {
    for value in ["", "TEST\u{1b}[31m", "TEST\nACCOUNT", "TEST CODE", "é"] {
        let mut state = code();
        state["user_code"] = value.into();
        assert_eq!(project(state).err(), Some(Failure("setup_screen_invalid")));
    }
    for url in [
        "https://auth.portoperatingsystem.com/SYNTHETIC-ACCOUNT/../device", // lint-rules: allow-literal inert URL-normalization refusal fixture; no socket uses this value
        "http://auth.portoperatingsystem.com/device", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "https://synthetic.invalid/device", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "https://auth.portoperatingsystem.com/device?token=SYNTHETIC-TOKEN", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "https://auth.portoperatingsystem.com/device#SYNTHETIC-ACCOUNT", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "https://user:password@auth.portoperatingsystem.com/device", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "https://auth.portoperatingsystem.com:444/device", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "https://auth.portoperatingsystem.com/other", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
    ] {
        let mut state = code();
        state["verification_uri"] = url.into();
        assert_eq!(project(state).err(), Some(Failure("setup_screen_invalid")));
    }
    for message in [
        "code: TEST\nurl: https://auth.openai.com/codex/device\nSYNTHETIC-ACCOUNT", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "code: TEST\nurl: https://auth.openai.com/codex/device", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
        "code: TEST\nurl: https://synthetic.invalid/\n", // lint-rules: allow-literal inert screen prompt fixture; HTTP observer only connects to loopback
    ] {
        let frame = ScreenFrame {
            seq: 1,
            state: UiState::Ready {
                message: message.into(),
            },
        };
        assert_eq!(
            prompt::project(frame, Provider::Chatgpt, 0).err(),
            Some(Failure("setup_screen_invalid"))
        );
    }
}

fn project(state: Value) -> Result<Option<super::Prompt>> {
    let frame = ScreenFrame {
        seq: 1,
        state: serde_json::from_value(state).map_err(|_| Failure("fixture"))?,
    };
    prompt::project(frame, Provider::Portos, 100)
}

#[test]
fn ignores_comments_accepts_multiline_data_and_requires_complete_ordered_frames() -> Result<()> {
    let mut events = Events::default();
    events.push(
        b": keepalive\r\nevent: message\r\ndata: {\"seq\":1,\r\ndata: \"state\":{\"state\":\"connected\"}}\r\n\r\n",
    )?;
    assert!(events.next(Provider::Chatgpt, 0)?.is_none());
    let bytes = wire(2, code());
    events.push(&bytes[..bytes.len() - 1])?;
    assert!(events.next(Provider::Portos, 100)?.is_none());
    events.push(b"\n")?;
    assert!(events.next(Provider::Portos, 100)?.is_some());
    assert_eq!(events.push(&wire(2, code())), Err(Failure("setup_screen_order")));
    for bytes in [
        b"data: {invalid}\n\n".as_slice(),
        b"data: \xff\n\n".as_slice(),
        b"data: {}\n\n".as_slice(),
    ] {
        assert_eq!(Events::default().push(bytes), Err(Failure("setup_screen_invalid")));
    }
    let mut events = Events::default();
    assert_eq!(events.push(&vec![b'x'; 65_537]), Err(Failure("setup_screen_limit")));
    Ok(())
}
