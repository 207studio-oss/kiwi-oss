use crate::compat::{self, Result};
use crate::process::JsonWorker;
use serde_json::json;
use std::io::Write;

pub struct StdioCodec {
    worker: Option<JsonWorker>,
    decoder: encoding_rs::Decoder,
    pending_cr: bool,
}

impl StdioCodec {
    pub fn new() -> Self {
        let setting = std::env::var("PYTHONIOENCODING").unwrap_or_default();
        let (label, errors) = setting.split_once(':').unwrap_or((&setting, "strict"));
        let native = matches!(errors, "" | "strict")
            && (label.is_empty()
                || ["utf8", "utf-8"]
                    .iter()
                    .any(|value| label.eq_ignore_ascii_case(value)));
        Self {
            worker: (!native).then(|| JsonWorker::new(include_str!("stdio_codec.py"))),
            decoder: compat::stdio_encoding()
                .1
                .new_decoder_without_bom_handling(),
            pending_cr: false,
        }
    }

    pub fn decode(&mut self, bytes: &[u8], final_chunk: bool) -> Result<String> {
        if let Some(worker) = &mut self.worker {
            return worker
                .call(json!({"operation":"decode","bytes":bytes,"final":final_chunk}))
                .and_then(|value| {
                    value.as_str().map(String::from).ok_or_else(|| {
                        format!(
                            "decoder should return a string result, not '{}'",
                            crate::protocol::type_name(&value)
                        )
                    })
                });
        }
        let mut text = String::with_capacity(bytes.len().saturating_mul(3).saturating_add(16));
        let (result, _) =
            self.decoder
                .decode_to_string_without_replacement(bytes, &mut text, final_chunk);
        if matches!(result, encoding_rs::DecoderResult::Malformed(..)) {
            return Err(compat::decode_stdio(bytes)
                .err()
                .unwrap_or_else(|| "invalid input encoding".into()));
        }
        let mut output = String::new();
        let mut chars = text.chars().peekable();
        if self.pending_cr && (!text.is_empty() || final_chunk) {
            output.push('\n');
            self.pending_cr = false;
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
        }
        while let Some(ch) = chars.next() {
            if ch == '\r' {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                    output.push('\n');
                } else if chars.peek().is_none() && !final_chunk {
                    self.pending_cr = true;
                } else {
                    output.push('\n');
                }
            } else {
                output.push(ch);
            }
        }
        Ok(output)
    }

    pub fn write(&mut self, output: &mut impl Write, text: &str) -> Result<()> {
        match &mut self.worker {
            Some(worker) => {
                let value = worker.call(json!({"operation":"encode","text":text}))?;
                let bytes: Vec<u8> =
                    serde_json::from_value(value).map_err(|error| error.to_string())?;
                output.write_all(&bytes).map_err(|error| error.to_string())
            }
            None => compat::write_stdio(output, text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_decoder_must_return_text() {
        let mut codec = StdioCodec::new();
        codec.worker = Some(JsonWorker::new("import sys\nfor line in sys.stdin.buffer:\n sys.stdout.buffer.write(b'{\"result\":42}\\n');sys.stdout.buffer.flush()"));
        assert_eq!(
            codec.decode(b"input", true),
            Err("decoder should return a string result, not 'int'".into())
        );
    }
}
