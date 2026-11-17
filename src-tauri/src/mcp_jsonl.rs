//! Memory MCP input compatibility. Responses always remain UTF-8 JSONL.
//! Decode complete byte frames before parsing, so invalid text cannot silently
//! end the connection without a JSON-RPC parse error.
use serde_json::{json, Value};
use std::borrow::Cow;
use std::io::{BufRead, Read, Write};

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const INVALID_ENCODING: &str = "Invalid MCP input encoding; send UTF-8 JSONL (GBK/GB18030 is also accepted).";
const UTF16_ENCODING: &str = "UTF-16 MCP input is unsupported; send UTF-8 JSONL (GBK/GB18030 is also accepted).";

fn decode_frame(bytes: &[u8]) -> Result<(Cow<'_, str>, bool), &'static str> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if bytes.starts_with(b"\xff\xfe") || bytes.starts_with(b"\xfe\xff") {
        return Err(UTF16_ENCODING);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok((Cow::Borrowed(text), false)),
        Err(_) => encoding_rs::GB18030
            .decode_without_bom_handling_and_without_replacement(bytes)
            .map(|text| (text, true))
            .ok_or(INVALID_ENCODING),
    }
}

fn parse_error(message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": message } })
}

fn write_response(output: &mut impl Write, response: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, response).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}

pub(crate) fn serve(
    mut input: impl BufRead,
    mut output: impl Write,
    mut dispatch: impl FnMut(Value) -> Option<Value>,
) -> Result<(), String> {
    let mut frame = Vec::new();
    let mut legacy_reported = false;
    loop {
        frame.clear();
        // Read one byte beyond the limit to detect oversized messages without
        // allocating an unbounded line or waiting for its eventual newline.
        let count = (&mut input).take((MAX_FRAME_BYTES + 1) as u64)
            .read_until(b'\n', &mut frame).map_err(|error| error.to_string())?;
        if count == 0 { return Ok(()); }
        if frame.len() > MAX_FRAME_BYTES {
            write_response(&mut output, &parse_error("MCP input frame exceeds the 1 MiB limit."))?;
            // Continuing in the middle of a discarded message is ambiguous.
            return Ok(());
        }
        let (text, legacy) = match decode_frame(&frame) {
            Ok(decoded) => decoded,
            Err(message) => {
                write_response(&mut output, &parse_error(message))?;
                // UTF-16 has a different newline width; do not attempt to
                // resynchronize it using the byte-oriented JSONL delimiter.
                if message == UTF16_ENCODING { return Ok(()); }
                continue;
            }
        };
        if legacy && !legacy_reported {
            eprintln!("[memory-mcp] accepted legacy GB18030 input; UTF-8 JSONL is recommended");
            legacy_reported = true;
        }
        if text.trim().is_empty() { continue; }
        let response = match serde_json::from_str::<Value>(&text) {
            Ok(request) => dispatch(request),
            Err(error) => Some(parse_error(&format!("parse error: {error}"))),
        };
        if let Some(response) = response { write_response(&mut output, &response)?; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    fn exchange(input: &[u8]) -> Vec<Value> {
        let mut output = Vec::new();
        serve(Cursor::new(input), &mut output, |request| {
            request.get("id").map(|id| json!({
                "jsonrpc": "2.0", "id": id, "result": request["params"]
            }))
        }).unwrap();
        String::from_utf8(output).unwrap().lines()
            .map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    #[test]
    fn utf8_bom_crlf_and_notifications_preserve_protocol() {
        let input = concat!("\u{feff}", "\r\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\r\n",
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"params\":{\"query\":\"中文与 emoji 🎮\"}}\r\n");
        let replies = exchange(input.as_bytes());
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["id"], 7);
        assert_eq!(replies[0]["result"]["query"], "中文与 emoji 🎮");
    }

    #[test]
    fn windows_gbk_request_recovers_the_original_chinese_text() {
        // Literal Windows CP936 bytes: this must not depend on the encoder
        // under test producing the fixture.
        let mut input = b"{\"id\":1,\"params\":{\"query\":\"".to_vec();
        input.extend_from_slice(b"\xd6\xd0\xce\xc4");
        input.extend_from_slice(b"\"}}\n");
        let replies = exchange(&input);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["result"]["query"], "中文");
    }

    #[test]
    fn gb18030_four_byte_characters_round_trip() {
        let request = "{\"id\":2,\"params\":{\"query\":\"游戏 🎮\"}}\n";
        let (encoded, _, errors) = encoding_rs::GB18030.encode(request);
        assert!(!errors);
        assert_eq!(exchange(&encoded)[0]["result"]["query"], "游戏 🎮");
    }

    #[test]
    fn malformed_encoding_returns_an_error_then_accepts_the_next_request() {
        let replies = exchange(b"\xff\n{\"id\":9,\"params\":{\"ok\":true}}\n");
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(replies[0]["error"]["message"], INVALID_ENCODING);
        assert_eq!(replies[1]["id"], 9);
        assert_eq!(replies[1]["result"]["ok"], true);
    }

    #[test]
    fn malformed_json_returns_an_error_without_losing_the_next_request() {
        let replies = exchange(b"{broken}\n{\"id\":3,\"params\":null}\n");
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(replies[1]["id"], 3);
    }

    #[test]
    fn utf16_bom_gets_one_explicit_error_instead_of_a_silent_disconnect() {
        for bom in [b"\xff\xfe".as_slice(), b"\xfe\xff".as_slice()] {
            let mut input = bom.to_vec();
            input.extend_from_slice(b"{\0}\0\n\0");
            let replies = exchange(&input);
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0]["error"]["message"], UTF16_ENCODING);
        }
    }

    #[test]
    fn oversized_frame_is_rejected_before_dispatch_and_closes_the_stream() {
        let input = vec![b' '; MAX_FRAME_BYTES + 20];
        let replies = exchange(&input);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert!(replies[0]["error"]["message"].as_str().unwrap().contains("1 MiB"));
    }

    #[test]
    fn fragmented_legacy_input_does_not_require_a_contiguous_read() {
        let mut input = b"{\"id\":4,\"params\":{\"query\":\"".to_vec();
        input.extend_from_slice(b"\xd6\xd0\xce\xc4");
        input.extend_from_slice(b"\"}}\n");
        let mut output = Vec::new();
        serve(BufReader::with_capacity(1, Cursor::new(input)), &mut output,
            |request| Some(json!({ "id": request["id"], "result": request["params"] }))).unwrap();
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["result"]["query"], "中文");
    }

    #[test]
    fn response_is_flushed_before_the_next_request_is_read() {
        use std::cell::Cell;
        use std::rc::Rc;
        struct CheckedReader { input: Cursor<Vec<u8>>, flushed: Rc<Cell<bool>> }
        impl Read for CheckedReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> { self.input.read(buffer) }
        }
        impl BufRead for CheckedReader {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                if self.input.position() == self.input.get_ref().len() as u64 {
                    assert!(self.flushed.get(), "response must be flushed before waiting on stdin");
                }
                self.input.fill_buf()
            }
            fn consume(&mut self, amount: usize) { self.input.consume(amount); }
        }
        struct CheckedWriter { bytes: Vec<u8>, flushed: Rc<Cell<bool>> }
        impl Write for CheckedWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.flushed.set(false);
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { self.flushed.set(true); Ok(()) }
        }
        let flushed = Rc::new(Cell::new(false));
        let input = CheckedReader { input: Cursor::new(b"{\"id\":1}\n".to_vec()), flushed: flushed.clone() };
        let output = CheckedWriter { bytes: Vec::new(), flushed };
        serve(input, output, Some).unwrap();
    }
}
