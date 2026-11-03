// SPDX-License-Identifier: MPL-2.0
//! Shared MCP 2025-06-18 stdio JSON-RPC helpers.

use crate::*;
use std::error::Error;
use std::fmt;
use std::process::ChildStdin;

pub(crate) const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_MCP_MESSAGE_BYTES: usize = 1024 * 1024;
static NEXT_MCP_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(crate) struct McpProtocolError(String);

impl fmt::Display for McpProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for McpProtocolError {}

#[derive(Debug)]
pub(crate) struct McpStdin {
    inner: Mutex<Option<ChildStdin>>,
}

impl McpStdin {
    pub(crate) fn new(stdin: ChildStdin) -> Self {
        Self {
            inner: Mutex::new(Some(stdin)),
        }
    }

    pub(crate) fn write(&self, value: &serde_json::Value) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        if body.len() > MAX_MCP_MESSAGE_BYTES {
            return Err(mcp_protocol_error(format!(
                "outbound MCP message exceeds {} byte limit",
                MAX_MCP_MESSAGE_BYTES
            )));
        }
        let mut guard = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let stdin = guard.as_mut().context("mcp stdin is closed")?;
        stdin.write_all(&body)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    pub(crate) fn close(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}

pub(crate) fn next_mcp_request_id() -> u64 {
    NEXT_MCP_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn spawn_mcp_reader<R: Read + Send + 'static>(
    reader: R,
) -> Receiver<Result<serde_json::Value>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            match read_mcp_message(&mut reader) {
                Ok(Some(value)) => {
                    if tx.send(Ok(value)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    let _ = tx.send(Err(err));
                    break;
                }
            }
        }
    });
    rx
}

pub(crate) fn send_mcp_request(
    stdin: &McpStdin,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<()> {
    stdin.write(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
}

pub(crate) fn send_mcp_notification(
    stdin: &McpStdin,
    method: &str,
    params: serde_json::Value,
) -> Result<()> {
    stdin.write(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }))
}

pub(crate) fn recv_mcp_response(
    stdin: &McpStdin,
    rx: &Receiver<Result<serde_json::Value>>,
    deadline: Instant,
    expected_id: u64,
    what: &str,
) -> Result<serde_json::Value> {
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let value = match rx.recv_timeout(timeout) {
            Ok(value) => value?,
            Err(mpsc::RecvTimeoutError::Timeout) => bail!("timed out waiting for {what}"),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(mcp_protocol_error(format!("MCP EOF before {what}")));
            }
        };
        let object = validate_jsonrpc_message(&value)?;
        if let Some(method) = object.get("method") {
            let method = method
                .as_str()
                .ok_or_else(|| mcp_protocol_error("MCP method must be a string"))?;
            if object.contains_key("result") || object.contains_key("error") {
                return Err(mcp_protocol_error(
                    "MCP request or notification must not contain result or error",
                ));
            }
            if object
                .get("params")
                .is_some_and(|params| !params.is_object() && !params.is_array())
            {
                return Err(mcp_protocol_error("MCP params must be an object or array"));
            }
            if let Some(id) = object.get("id") {
                validate_mcp_id(id)?;
                let response = if method == "ping" {
                    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}})
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": "Method not found"},
                    })
                };
                stdin.write(&response)?;
            }
            continue;
        }

        let id = object
            .get("id")
            .ok_or_else(|| mcp_protocol_error("MCP response is missing id"))?;
        if id.as_u64() != Some(expected_id) {
            return Err(mcp_protocol_error(format!(
                "MCP response id does not match request id {expected_id}"
            )));
        }
        match (object.get("result"), object.get("error")) {
            (Some(result), None) => return Ok(result.clone()),
            (None, Some(error)) => {
                let error = error
                    .as_object()
                    .ok_or_else(|| mcp_protocol_error("MCP JSON-RPC error must be an object"))?;
                let code = error
                    .get("code")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or_else(|| {
                        mcp_protocol_error("MCP JSON-RPC error is missing integer code")
                    })?;
                let message = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| mcp_protocol_error("MCP JSON-RPC error is missing message"))?;
                return Err(mcp_protocol_error(format!(
                    "MCP JSON-RPC error {code}: {message}"
                )));
            }
            _ => {
                return Err(mcp_protocol_error(
                    "MCP response must contain exactly one of result or error",
                ));
            }
        }
    }
}

pub(crate) fn is_mcp_protocol_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<McpProtocolError>().is_some()
}

pub(crate) fn mcp_protocol_error(message: impl Into<String>) -> anyhow::Error {
    McpProtocolError(message.into()).into()
}

fn read_mcp_message<R: BufRead>(reader: &mut R) -> Result<Option<serde_json::Value>> {
    // Grow only as bytes arrive. Preallocating the full limit would reserve
    // attacker-controlled inbound payload storage before the limit is enforced.
    let mut body = Vec::new();
    let mut pending_carriage_return = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if body.is_empty() && !pending_carriage_return {
                Ok(None)
            } else {
                Err(mcp_protocol_error("unterminated MCP message"))
            };
        }
        let mut consumed = 0;
        for byte in buffer {
            consumed += 1;
            if *byte == b'\n' {
                reader.consume(consumed);
                if body.is_empty() {
                    return Err(mcp_protocol_error("blank MCP message"));
                }
                let text = String::from_utf8(body)
                    .map_err(|_| mcp_protocol_error("MCP message is not UTF-8"))?;
                let value = serde_json::from_str(&text).map_err(|error| {
                    mcp_protocol_error(format!("invalid MCP JSON payload: {error}"))
                })?;
                validate_jsonrpc_message(&value)?;
                return Ok(Some(value));
            }
            if pending_carriage_return {
                push_mcp_byte(&mut body, b'\r')?;
                pending_carriage_return = false;
            }
            if *byte == b'\r' {
                pending_carriage_return = true;
            } else {
                push_mcp_byte(&mut body, *byte)?;
            }
        }
        reader.consume(consumed);
    }
}

fn push_mcp_byte(body: &mut Vec<u8>, byte: u8) -> Result<()> {
    if body.len() == MAX_MCP_MESSAGE_BYTES {
        return Err(mcp_protocol_error(format!(
            "inbound MCP message exceeds {} byte limit",
            MAX_MCP_MESSAGE_BYTES
        )));
    }
    body.push(byte);
    Ok(())
}

fn validate_jsonrpc_message(
    value: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>> {
    let object = value
        .as_object()
        .ok_or_else(|| mcp_protocol_error("MCP message must be a JSON-RPC object"))?;
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(mcp_protocol_error("MCP message must declare jsonrpc '2.0'"));
    }
    Ok(object)
}

fn validate_mcp_id(id: &serde_json::Value) -> Result<()> {
    if id.as_u64().is_some() || id.as_str().is_some_and(|value| !value.is_empty()) {
        Ok(())
    } else {
        Err(mcp_protocol_error(
            "MCP request id must be a non-empty string or unsigned integer",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read_message(bytes: &[u8]) -> Result<Option<serde_json::Value>> {
        read_mcp_message(&mut BufReader::new(Cursor::new(bytes)))
    }

    #[test]
    fn reads_lf_and_crlf_delimited_jsonrpc_messages() {
        for delimiter in [b"\n".as_slice(), b"\r\n".as_slice()] {
            let mut message = br#"{"jsonrpc":"2.0","method":"notice"}"#.to_vec();
            message.extend_from_slice(delimiter);
            assert_eq!(
                read_message(&message).expect("message"),
                Some(serde_json::json!({"jsonrpc":"2.0","method":"notice"}))
            );
        }
    }

    #[test]
    fn rejects_invalid_mcp_stdio_messages() {
        for bytes in [
            b"\n".as_slice(),
            b"not-json\n".as_slice(),
            b"{\"jsonrpc\":\"2.0\"} trailing\n".as_slice(),
            b"{\"jsonrpc\":\"2.0\",\"method\":\"notice\"".as_slice(),
            b"\xff\n".as_slice(),
        ] {
            assert!(read_message(bytes).is_err(), "{bytes:?} should fail");
        }
        let mut oversized = vec![b'x'; MAX_MCP_MESSAGE_BYTES + 2];
        oversized.push(b'\n');
        assert!(read_message(&oversized).is_err());
    }
}
