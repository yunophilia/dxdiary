//! LSP wire framing.
//!
//! Messages are `Content-Length: N\r\n\r\n<N bytes of JSON>`. Deliberately
//! generic over `Read`/`Write` rather than tied to a child process, so the
//! framing — where the real bugs live — is testable without spawning anything.

use std::io::{BufRead, Read, Write};

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Refuse absurd headers rather than trying to allocate them.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Read one message. `Ok(None)` means the peer closed cleanly.
pub fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>> {
    let mut length: Option<usize> = None;

    // Headers, terminated by a blank line.
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            // EOF between messages is a clean shutdown; mid-headers is not.
            return if length.is_none() {
                Ok(None)
            } else {
                bail!("stream ended after Content-Length but before the body")
            };
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }

        // Header names are case-insensitive per the spec.
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                let n: usize = value
                    .trim()
                    .parse()
                    .with_context(|| format!("bad Content-Length: {value:?}"))?;
                if n > MAX_MESSAGE_BYTES {
                    bail!("Content-Length {n} exceeds the {MAX_MESSAGE_BYTES} byte limit");
                }
                length = Some(n);
            }
            // Content-Type is the only other header, and carries nothing we need.
        }
    }

    let Some(length) = length else {
        bail!("message had no Content-Length header");
    };

    // read_exact rather than read: a pipe can hand back a partial buffer, and
    // treating that as the whole message is the classic framing bug.
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .context("stream ended mid-message")?;

    Ok(Some(
        serde_json::from_slice(&body).context("message body was not valid JSON")?,
    ))
}

/// Write one message with its header.
pub fn write_message<W: Write>(writer: &mut W, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    // Servers block until the whole message arrives; an unflushed write is a
    // deadlock, not a delay.
    writer.flush()?;
    Ok(())
}

/// Drain whatever a server wrote to stderr, for error reporting.
pub fn drain_stderr<R: Read>(mut reader: R) -> String {
    let mut buf = String::new();
    let _ = reader.read_to_string(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn framed(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn round_trips_a_message() {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        let mut reader = Cursor::new(buf);
        assert_eq!(read_message(&mut reader).unwrap(), Some(msg));
    }

    #[test]
    fn reads_several_messages_from_one_stream() {
        let mut buf = Vec::new();
        for i in 0..3 {
            write_message(&mut buf, &json!({"id": i})).unwrap();
        }
        let mut reader = Cursor::new(buf);
        for i in 0..3 {
            let msg = read_message(&mut reader).unwrap().unwrap();
            assert_eq!(msg["id"], i);
        }
        assert_eq!(read_message(&mut reader).unwrap(), None, "then EOF");
    }

    #[test]
    fn a_clean_close_is_not_an_error() {
        let mut reader = Cursor::new(Vec::new());
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let body = r#"{"ok":true}"#;
        let raw = format!("content-length: {}\r\n\r\n{body}", body.len());
        let mut reader = Cursor::new(raw.into_bytes());
        assert_eq!(read_message(&mut reader).unwrap().unwrap()["ok"], true);
    }

    #[test]
    fn an_extra_header_is_ignored() {
        let body = r#"{"ok":true}"#;
        let raw = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{body}",
            body.len()
        );
        let mut reader = Cursor::new(raw.into_bytes());
        assert!(read_message(&mut reader).unwrap().is_some());
    }

    #[test]
    fn a_body_is_measured_in_bytes_not_characters() {
        // Multi-byte content: a character-based length would truncate here.
        let msg = json!({"text": "héllo wörld — ✓"});
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        let mut reader = Cursor::new(buf);
        assert_eq!(read_message(&mut reader).unwrap(), Some(msg));
    }

    #[test]
    fn a_truncated_body_is_an_error_not_a_silent_partial_read() {
        let mut raw = framed(r#"{"id":1,"padding":"aaaaaaaaaa"}"#);
        raw.truncate(raw.len() - 5);
        let mut reader = Cursor::new(raw);
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn a_missing_content_length_is_rejected() {
        let mut reader = Cursor::new(b"X-Nonsense: 1\r\n\r\n{}".to_vec());
        let err = read_message(&mut reader).unwrap_err().to_string();
        assert!(err.contains("Content-Length"), "{err}");
    }

    #[test]
    fn a_non_numeric_content_length_is_rejected() {
        let mut reader = Cursor::new(b"Content-Length: banana\r\n\r\n{}".to_vec());
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn an_absurd_content_length_is_refused_rather_than_allocated() {
        let mut reader = Cursor::new(b"Content-Length: 999999999999\r\n\r\n".to_vec());
        let err = read_message(&mut reader).unwrap_err().to_string();
        assert!(err.contains("limit"), "{err}");
    }

    #[test]
    fn a_malformed_body_reports_json_rather_than_framing() {
        let mut reader = Cursor::new(framed("not json at all"));
        let err = read_message(&mut reader).unwrap_err().to_string();
        assert!(err.contains("JSON"), "{err}");
    }
}
