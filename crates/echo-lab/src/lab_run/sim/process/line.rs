//! Cancellation-safe bounded JSONL. Pending bytes belong to the process owner.
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

pub(super) async fn read(output: &mut (impl AsyncRead + Unpin), pending: &mut Vec<u8>) -> Result<Value, String> {
    loop {
        let byte = output
            .read_u8()
            .await
            .map_err(|_| "process: exited before observation")?;
        if pending.len() >= 4096 {
            return Err("process: oversized observation".into());
        }
        if byte == b'\n' {
            let bytes = std::mem::take(pending);
            return serde_json::from_slice(&bytes).map_err(|_| "process: malformed observation".into());
        }
        pending.push(byte);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::{Future, poll_fn},
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;
    struct Input(Vec<u8>);
    impl AsyncRead for Input {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.0.is_empty() {
                return Poll::Pending;
            }
            let count = buffer.remaining().min(self.0.len());
            buffer.put_slice(&self.0[..count]);
            self.0.drain(..count);
            Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn cancelled_jsonl_read_resumes_exact_partial_record_and_next_record() {
        let mut input = Input(br#"{"event":"provider."#.to_vec());
        let mut pending = vec![];
        {
            let mut line = Box::pin(read(&mut input, &mut pending));
            poll_fn(|cx| {
                assert!(line.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        assert_eq!(pending, br#"{"event":"provider."#);
        input.0.extend_from_slice(b"append\"}\n{\"event\":\"next\"}\n");
        assert_eq!(
            read(&mut input, &mut pending).await.unwrap(),
            serde_json::json!({"event":"provider.append"})
        );
        assert_eq!(
            read(&mut input, &mut pending).await.unwrap(),
            serde_json::json!({"event":"next"})
        );
        assert_eq!(pending, b"");
    }
    #[tokio::test]
    async fn malformed_oversized_and_incomplete_observations_keep_named_errors() {
        for (bytes, expected) in [
            (b"not-json\n".to_vec(), "process: malformed observation"),
            (vec![b'x'; 4097], "process: oversized observation"),
            (b"{\"event\":".to_vec(), "process: exited before observation"),
        ] {
            assert_eq!(read(&mut bytes.as_slice(), &mut vec![]).await, Err(expected.into()));
        }
    }
}
