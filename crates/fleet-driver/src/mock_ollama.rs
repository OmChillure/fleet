//! In-process Ollama stand-in for tests. No network provider, no model SKU.
//!
//! Serves OpenAI-compatible `POST /v1/chat/completions` on `127.0.0.1:0`.
//! The fixture model id is [`TEST_MODEL`] — a label, not a vendor version.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use serde_json::{json, Value};

/// Fixture model name used by tests. Not a real Ollama tag and not a
/// cloud SKU — the mock accepts any id and echoes this as the default.
pub const TEST_MODEL: &str = "tiny";

pub struct MockOllama {
    pub addr: SocketAddr,
    rounds: Arc<AtomicU32>,
    _join: JoinHandle<()>,
}

impl MockOllama {
    pub fn spawn() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let rounds = Arc::new(AtomicU32::new(0));
        let rounds2 = rounds.clone();
        let join = thread::spawn(move || serve(listener, rounds2));
        Ok(Self {
            addr,
            rounds,
            _join: join,
        })
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn rounds(&self) -> u32 {
        self.rounds.load(Ordering::SeqCst)
    }
}

fn serve(listener: TcpListener, rounds: Arc<AtomicU32>) {
    listener.set_nonblocking(false).ok();
    while let Ok((mut stream, _)) = listener.accept() {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            match stream.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(hdr_end) = find_double_crlf(&buf) {
                        if let Some(len) = content_length(&buf[..hdr_end]) {
                            if buf.len() >= hdr_end + len {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    if buf.len() > 256 * 1024 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let raw = String::from_utf8_lossy(&buf);
        if !raw.contains("POST") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            continue;
        }
        let n = rounds.fetch_add(1, Ordering::SeqCst);
        let body = reply_body(n, &raw);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
    }
}

/// First turn: write `work/out.txt` via the tool loop. Second: finish.
fn reply_body(round: u32, request: &str) -> String {
    let model = request_model(request).unwrap_or_else(|| TEST_MODEL.to_string());
    if round == 0 {
        json!({
            "id": "mock-1",
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "write_file",
                            "arguments": "{\"path\":\"work/out.txt\",\"content\":\"from-tiny\"}"
                        }
                    }]
                }
            }]
        })
        .to_string()
    } else {
        json!({
            "id": "mock-2",
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Task completed successfully with artifacts."
                }
            }]
        })
        .to_string()
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn content_length(headers: &[u8]) -> Option<usize> {
    let s = String::from_utf8_lossy(headers);
    for line in s.split("\r\n") {
        let (k, v) = line.split_once(':')?;
        if k.eq_ignore_ascii_case("content-length") {
            return v.trim().parse().ok();
        }
    }
    None
}

fn request_model(raw: &str) -> Option<String> {
    let start = raw.find("\r\n\r\n").map(|i| i + 4)?;
    let v: Value = serde_json::from_str(raw[start..].trim()).ok()?;
    v.get("model")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_is_not_a_vendor_sku() {
        assert_eq!(TEST_MODEL, "tiny");
        assert!(!TEST_MODEL.contains("grok"));
        assert!(!TEST_MODEL.contains("gpt"));
        assert!(!TEST_MODEL.contains("sonnet"));
        assert!(!TEST_MODEL.contains('.'));
    }
}
