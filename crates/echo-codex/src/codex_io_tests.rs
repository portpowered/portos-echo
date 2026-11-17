use super::*;
use serde_json::json;
use std::{error::Error, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream, duplex, split};

type TestIo = RpcIo<tokio::io::ReadHalf<DuplexStream>, tokio::io::WriteHalf<DuplexStream>>;

fn session(capacity: usize, limits: Limits) -> Result<(TestIo, DuplexStream), RpcError> {
    let (client, peer) = duplex(capacity);
    let (reader, writer) = split(client);
    Ok((RpcIo::new(reader, writer, limits)?, peer))
}

async fn line(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Result<Value, Box<dyn Error>> {
    let mut line = String::new();
    assert_ne!(reader.read_line(&mut line).await?, 0);
    Ok(serde_json::from_str(&line)?)
}

#[tokio::test(start_paused = true)]
async fn out_of_order_replies_and_server_request_share_cancellation_safe_io() -> Result<(), Box<dyn Error>> {
    let (mut io, peer) = session(4096, Limits::default())?;
    let first = io.begin_request("first", &json!({"x": 1}))?;
    let second = io.begin_request("second", &())?;
    let peer = async {
        let (read, mut write) = split(peer);
        let mut reader = BufReader::new(read);
        assert_eq!(
            line(&mut reader).await?,
            json!({"id":1,"method":"first","params":{"x":1}})
        );
        assert_eq!(
            line(&mut reader).await?,
            json!({"id":2,"method":"second","params":null})
        );
        write
            .write_all(b"{\"id\":2,\"result\":\"second\"}\n{\"id\":1,\"result\":\"first\"}\n")
            .await?;
        write.write_all(b"{\"id\":1,\"result\":\"duplicate\"}\n").await?;
        write
            .write_all(b"{\"method\":\"future\",\"params\":{\"x\":2},\"extra\":true}\n")
            .await?;
        write
            .write_all(b"{\"id\":\"tool\",\"method\":\"item/tool/call\",\"params\":{}}\n")
            .await?;
        assert_eq!(line(&mut reader).await?, json!({"id":"tool","result":{"success":true}}));
        Ok::<_, Box<dyn Error>>(())
    };
    let caller = async {
        assert_eq!(io.wait_reply(&second).await?, json!("second"));
        assert_eq!(io.wait_reply(&first).await?, json!("first"));
        let Message::Notification { raw, .. } = io.next_event().await? else {
            return Err(RpcError::Malformed);
        };
        assert_eq!(raw, json!({"method":"future","params":{"x":2},"extra":true}));
        let Message::Request { id, method, .. } = io.next_event().await? else {
            return Err(RpcError::Malformed);
        };
        assert_eq!(method, "item/tool/call");
        io.respond(&id, &json!({"success":true}))?;
        io.flush().await?;
        Ok::<_, RpcError>(())
    };
    let (peer, caller) = tokio::join!(peer, caller);
    peer?;
    caller?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn idle_deadline_settles_once_and_late_reply_cannot_complete_another_call() -> Result<(), Box<dyn Error>> {
    let (mut io, mut peer) = session(4096, Limits::default())?;
    let old = io.begin_request("old", &())?;
    io.flush().await?;
    assert_eq!(io.wait_reply(&old).await, Err(RpcError::Deadline));
    assert_eq!(io.wait_reply(&old).await, Err(RpcError::UnknownRequest));
    let new = io.begin_request("new", &())?;
    peer.write_all(b"{\"id\":1,\"result\":\"late\"}\n{\"id\":2,\"result\":\"new\"}\n")
        .await?;
    assert_eq!(io.wait_reply(&new).await?, json!("new"));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn blocked_partial_write_has_a_deadline_and_closes_both_halves() -> Result<(), Box<dyn Error>> {
    let (mut io, mut peer) = session(1, Limits::default())?;
    let id = io.begin_request("blocked", &())?;
    assert_eq!(io.wait_reply(&id).await, Err(RpcError::Deadline));
    assert_eq!(io.flush().await, Err(RpcError::Deadline));
    let mut written = Vec::new();
    peer.read_to_end(&mut written).await?;
    assert_eq!(written, b"{");
    assert_eq!(
        peer.write_all(b"x").await.err().map(|e| e.kind()),
        Some(std::io::ErrorKind::BrokenPipe)
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn unsent_cancelled_or_expired_calls_are_never_written() -> Result<(), Box<dyn Error>> {
    let (mut io, peer) = session(4096, Limits::default())?;
    let cancelled = io.begin_request("cancelled", &())?;
    assert!(io.cancel(&cancelled));
    assert!(!io.cancel(&cancelled));
    let expired = io.begin_request("expired", &())?;
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(io.wait_reply(&expired).await, Err(RpcError::Deadline));
    let live = io.begin_request("live", &())?;
    io.flush().await?;
    let mut reader = BufReader::new(peer);
    assert_eq!(line(&mut reader).await?, json!({"id":3,"method":"live","params":null}));
    assert_eq!(io.wait_reply(&live).await, Err(RpcError::Deadline));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_partial_write_keeps_its_framing_but_not_its_waiter() -> Result<(), Box<dyn Error>> {
    let (mut io, mut peer) = session(1, Limits::default())?;
    let abandoned = io.begin_request("abandon", &())?;
    io.progress().await?;
    assert!(io.cancel(&abandoned));
    let next = io.begin_request("next", &())?;
    let caller = async {
        assert_eq!(io.wait_reply(&abandoned).await, Err(RpcError::UnknownRequest));
        assert_eq!(io.wait_reply(&next).await?, json!("next"));
        Ok::<_, RpcError>(())
    };
    let peer = async {
        let mut reader = BufReader::new(&mut peer);
        assert_eq!(
            line(&mut reader).await?,
            json!({"id":1,"method":"abandon","params":null})
        );
        assert_eq!(line(&mut reader).await?, json!({"id":2,"method":"next","params":null}));
        reader
            .get_mut()
            .write_all(b"{\"id\":1,\"result\":\"late\"}\n{\"id\":2,\"result\":\"next\"}\n")
            .await?;
        Ok::<_, Box<dyn Error>>(())
    };
    let (caller, peer) = tokio::join!(caller, peer);
    caller?;
    peer?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn oversized_and_malformed_input_and_eof_fail_every_pending_call() -> Result<(), Box<dyn Error>> {
    for (bytes, expected) in [
        (vec![b'x'; 129], RpcError::LineTooLong),
        (b"invalid\n".to_vec(), RpcError::Malformed),
        (b"{\"id\":".to_vec(), RpcError::Disconnected),
    ] {
        let (mut io, mut peer) = session(
            4096,
            Limits {
                line_bytes: 128,
                ..Limits::default()
            },
        )?;
        let a = io.begin_request("a", &())?;
        let b = io.begin_request("b", &())?;
        peer.write_all(&bytes).await?;
        peer.shutdown().await?;
        assert_eq!(io.wait_reply(&a).await, Err(expected));
        assert_eq!(io.wait_reply(&b).await, Err(expected));
        assert_eq!(io.notify("initialized"), Err(expected));
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn accepted_maximum_line_survives_fragmentation_and_cancelled_waits() -> Result<(), Box<dyn Error>> {
    let limits = Limits {
        line_bytes: 128,
        ..Limits::default()
    };
    let (mut io, mut peer) = session(4096, limits)?;
    let raw = json!({"method":"future","params":"x".repeat(97)});
    let bytes = serde_json::to_vec(&raw)?;
    assert_eq!(bytes.len(), limits.line_bytes);
    peer.write_all(&bytes[..64]).await?;
    io.progress().await?;
    tokio::select! {
        biased;
        _ = io.next_event() => return Err("partial line must not emit an event".into()),
        () = tokio::time::sleep(Duration::from_secs(1)) => {}
    }
    peer.write_all(&bytes[64..]).await?;
    peer.write_all(b"\n").await?;
    let Message::Notification { raw: observed, .. } = io.next_event().await? else {
        return Err("expected notification".into());
    };
    assert_eq!(observed, raw);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn overflow_is_terminal_and_previously_accepted_events_remain_drainable() -> Result<(), Box<dyn Error>> {
    let (mut io, mut peer) = session(
        4096,
        Limits {
            notifications: 1,
            ..Limits::default()
        },
    )?;
    let id = io.begin_request("pending", &())?;
    peer.write_all(b"{\"method\":\"one\"}\n{\"method\":\"two\"}\n").await?;
    assert_eq!(io.wait_reply(&id).await, Err(RpcError::EventsFull));
    let Message::Notification { method, .. } = io.next_event().await? else {
        return Err("expected accepted event".into());
    };
    assert_eq!(method, "one");
    assert_eq!(io.next_event().await.err(), Some(RpcError::EventsFull));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn output_admission_includes_notifications_and_does_not_consume_rejected_tool_reply() -> Result<(), Box<dyn Error>>
{
    let limits = Limits {
        pending_requests: 1,
        ..Limits::default()
    };
    let (mut io, mut peer) = session(4096, limits)?;
    peer.write_all(b"{\"id\":\"tool\",\"method\":\"item/tool/call\",\"params\":{}}\n")
        .await?;
    let Message::Request { id, .. } = io.next_event().await? else {
        return Err("expected server request".into());
    };
    io.notify("initialized")?;
    assert_eq!(io.notify("extra"), Err(RpcError::PendingFull));
    assert_eq!(io.begin_request("extra", &()).err(), Some(RpcError::PendingFull));
    assert_eq!(io.respond(&id, &()), Err(RpcError::PendingFull));
    io.flush().await?;
    io.respond(&id, &json!({"success":true}))?;
    io.flush().await?;
    let mut reader = BufReader::new(peer);
    assert_eq!(line(&mut reader).await?, json!({"method":"initialized"}));
    assert_eq!(line(&mut reader).await?, json!({"id":"tool","result":{"success":true}}));
    assert_eq!(io.respond(&id, &()), Err(RpcError::UnknownRequest));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn io_failure_is_payload_free_and_earlier_completion_survives_close() -> Result<(), Box<dyn Error>> {
    let (mut io, mut peer) = session(4096, Limits::default())?;
    let a = io.begin_request("a", &())?;
    let b = io.begin_request("b", &())?;
    peer.write_all(
        b"{\"id\":1,\"error\":{\"code\":-9,\"message\":\"sensitive text\"}}\n{\"method\":\"reply-observed\"}\n",
    )
    .await?;
    let Message::Notification { method, .. } = io.next_event().await? else {
        return Err("reply barrier expected".into());
    };
    assert_eq!(method, "reply-observed");
    io.close(RpcError::Disconnected);
    io.close(RpcError::Malformed);
    assert_eq!(io.wait_reply(&a).await, Err(RpcError::Remote(-9)));
    assert_eq!(io.wait_reply(&b).await, Err(RpcError::Disconnected));
    assert_eq!(format!("{}", RpcError::Remote(-9)), "Codex RPC: Remote(-9)");
    let (mut broken, peer) = session(1, Limits::default())?;
    drop(peer);
    let id = broken.begin_request("broken", &())?;
    assert_eq!(broken.wait_reply(&id).await, Err(RpcError::Disconnected));
    Ok(())
}

enum FaultyWriter {
    Zero,
    Write,
    Flush,
}

impl AsyncWrite for FaultyWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(match *self {
            Self::Zero => Ok(0),
            Self::Write => Err(std::io::Error::other("synthetic private write diagnostic")),
            Self::Flush => Ok(bytes.len()),
        })
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other("synthetic private flush diagnostic")))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.poll_flush(context)
    }
}

struct FaultyReader;

impl AsyncRead for FaultyReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other("synthetic private read diagnostic")))
    }
}

#[tokio::test(start_paused = true)]
async fn write_zero_read_write_and_flush_errors_close_without_exposing_io_diagnostics() -> Result<(), Box<dyn Error>> {
    for writer in [FaultyWriter::Zero, FaultyWriter::Write, FaultyWriter::Flush] {
        let (reader, _held_open) = duplex(1);
        let mut io = RpcIo::new(reader, writer, Limits::default())?;
        let id = io.begin_request("failure", &())?;
        let error = io.wait_reply(&id).await.err().ok_or("expected failure")?;
        assert_eq!(error, RpcError::Disconnected);
        assert_eq!(error.to_string(), "Codex RPC: Disconnected");
        assert_eq!(io.next_event().await.err(), Some(RpcError::Disconnected));
    }
    let mut io = RpcIo::new(FaultyReader, tokio::io::sink(), Limits::default())?;
    let id = io.begin_request("read-failure", &())?;
    assert_eq!(io.wait_reply(&id).await, Err(RpcError::Disconnected));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn expired_output_closes_before_sending_a_queued_notification() -> Result<(), Box<dyn Error>> {
    let (mut io, mut peer) = session(4096, Limits::default())?;
    io.notify("initialized")?;
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(io.flush().await, Err(RpcError::Deadline));
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).await?;
    assert_eq!(bytes, b"");
    assert_eq!(io.begin_request("late", &()).err(), Some(RpcError::Deadline));
    Ok(())
}
