//! PROVISIONAL completed-item SSE path, pinned to Codex ee344ca9458351a6ea102795fa73110ad3ede5dc.
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Decoder {
    pending: Vec<u8>,
    data: Vec<String>,
    items: BTreeMap<u64, Value>,
    completed: bool,
}

impl Decoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=end).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line[..end]).map_err(|_| "invalid model response")?;
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                self.frame()?;
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data.push(data.strip_prefix(' ').unwrap_or(data).into());
            }
        }
        Ok(())
    }

    fn frame(&mut self) -> Result<(), &'static str> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data).join("\n");
        let event: Value = serde_json::from_str(&data).map_err(|_| "invalid model response")?;
        if self.completed {
            return Err("invalid model response");
        }
        match event["type"].as_str().ok_or("invalid model response")? {
            "response.output_item.done" => {
                let index = event["output_index"].as_u64().ok_or("invalid model response")?;
                let item = event
                    .get("item")
                    .filter(|item| item.is_object())
                    .ok_or("invalid model response")?;
                if self.items.insert(index, item.clone()).is_some() {
                    return Err("invalid model response");
                }
            }
            "response.completed" => {
                if event["response"]["status"] != "completed" {
                    return Err("invalid model response");
                }
                self.completed = true;
            }
            "response.failed" | "response.incomplete" | "error" => return Err("invalid model response"),
            _ => (),
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<Vec<Value>, &'static str> {
        if !self.completed || !self.pending.is_empty() || !self.data.is_empty() {
            return Err("invalid model response");
        }
        if self.items.keys().copied().ne(0..self.items.len() as u64) {
            return Err("invalid model response");
        }
        Ok(self.items.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(event: &Value) -> String {
        format!("data: {event}\r\n\r\n")
    }
    fn completed() -> String {
        frame(&json!({"type":"response.completed", "response":{"status":"completed"}}))
    }
    fn item(index: u64, item: &Value) -> String {
        frame(&json!({"type":"response.output_item.done", "output_index":index, "item":item}))
    }

    #[test]
    fn fragmented_utf8_crlf_multiline_data_and_opaque_items() {
        let values = vec![
            json!({"type":"reasoning", "encrypted_content":"opaque", "summary":[]}),
            json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"世界"}]}),
        ];
        let mut stream =
            String::from(": comment\r\nevent: unrelated\r\ndata: {\"type\":\r\ndata: \"response.created\"}\r\n\r\n");
        stream.push_str(&frame(&json!({"type":"response.output_text.delta", "delta":"ignored"})));
        // Arrival may be out of index order; history is in output order.
        stream.push_str(&item(1, &values[1]));
        stream.push_str(&item(0, &values[0]));
        stream.push_str(&completed());
        for size in 1..=stream.len() {
            let mut decoder = Decoder::default();
            for chunk in stream.as_bytes().chunks(size) {
                decoder.push(chunk).unwrap();
            }
            assert_eq!(decoder.finish().unwrap(), values);
        }
    }

    #[test]
    fn rejects_error_duplicate_incomplete_and_truncated_frames() {
        for stream in [
            "data: nope\n\n".into(),
            "data: {}\n\n".into(),
            "data: {\"type\":\"response.created\"}\n\n".into(),
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"incomplete\"}}\n\n".into(),
            frame(&json!({"type":"response.failed"})),
            frame(&json!({"type":"response.incomplete"})),
            frame(&json!({"type":"error"})),
            frame(&json!({"type":"response.output_item.done", "output_index":0, "item":null})),
            frame(&json!({"type":"response.output_item.done", "item":{}})),
            format!("{}{}{}", item(0, &json!({})), item(0, &json!({})), completed()),
            format!("{}{}", item(1, &json!({})), completed()),
            format!("{}{}", completed(), completed()),
            format!("{}data: {{}}", completed()),
            format!("{}data: {{}}\n", completed()),
        ] {
            let mut decoder = Decoder::default();
            let result = decoder.push(stream.as_bytes()).and_then(|()| decoder.finish());
            assert_eq!(result, Err("invalid model response"), "{stream}");
        }
        let mut decoder = Decoder::default();
        assert_eq!(decoder.push(&[255, b'\n']), Err("invalid model response"));
    }
}
