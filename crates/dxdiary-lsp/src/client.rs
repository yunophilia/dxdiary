//! Talking to a language server.
//!
//! No async runtime. The server runs as a child process with a reader thread
//! feeding a channel, matching the pattern blame already uses — the UI polls,
//! nothing blocks a frame. Adding tokio for one subprocess would be a large
//! dependency for no benefit in a synchronous event loop.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use anyhow::{Context, Result};
use lsp_types::Diagnostic;
use serde_json::{json, Value};

use crate::protocol::{read_message, write_message};
use crate::registry::ServerSpec;

/// Something the server told us, delivered to the UI thread.
#[derive(Debug, Clone)]
pub enum Event {
    /// `textDocument/publishDiagnostics`.
    Diagnostics { uri: String, items: Vec<Diagnostic> },
    /// A reply to a request we sent.
    Response { id: i64, result: Value },
    /// The server reported an error for one of our requests.
    Error { id: i64, message: String },
    /// The server exited or the pipe broke.
    Closed(String),
}

pub struct Client {
    child: Child,
    outgoing: Sender<Value>,
    pub events: Receiver<Event>,
    next_id: Arc<AtomicI64>,
    pub spec: &'static ServerSpec,
    /// Version of every document the server has been told about, by URI.
    ///
    /// The protocol requires versions to increase per document, and a server
    /// uses them to discard diagnostics computed against text the client has
    /// since replaced. Tracked here rather than by the caller so that the
    /// invariant cannot be broken from outside.
    versions: RefCell<HashMap<String, i32>>,
}

impl Client {
    /// Spawn a server and complete the LSP handshake.
    pub fn spawn(spec: &'static ServerSpec, root: &std::path::Path) -> Result<Self> {
        let mut command = Command::new(spec.command);
        command.args(spec.args);
        Self::from_command(command, spec, root)
    }

    /// Same, from a prepared command.
    ///
    /// Exists so the client can be tested against a mock server: no real
    /// language server is guaranteed to be installed anywhere, and a protocol
    /// client that is only ever exercised by hand is a client that breaks.
    pub fn from_command(
        mut command: Command,
        spec: &'static ServerSpec,
        root: &std::path::Path,
    ) -> Result<Self> {
        let mut child = command
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Servers are chatty on stderr; letting it inherit would scribble
            // over the TUI.
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {}", spec.command))?;

        let stdin = child.stdin.take().context("server stdin")?;
        let stdout = child.stdout.take().context("server stdout")?;

        let (out_tx, out_rx) = mpsc::channel::<Value>();
        let (ev_tx, ev_rx) = mpsc::channel::<Event>();

        // Writer thread: serialises everything we send, so callers never block
        // on a slow server.
        std::thread::spawn(move || {
            let mut w = BufWriter::new(stdin);
            while let Ok(msg) = out_rx.recv() {
                if write_message(&mut w, &msg).is_err() {
                    break;
                }
            }
        });

        // Reader thread: turns the wire into `Event`s.
        let reader_tx = ev_tx.clone();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                match read_message(&mut r) {
                    Ok(Some(msg)) => {
                        if let Some(event) = to_event(&msg) {
                            if reader_tx.send(event).is_err() {
                                break; // UI is gone
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = reader_tx.send(Event::Closed("server exited".into()));
                        break;
                    }
                    Err(e) => {
                        let _ = reader_tx.send(Event::Closed(e.to_string()));
                        break;
                    }
                }
            }
        });

        let client = Client {
            child,
            outgoing: out_tx,
            events: ev_rx,
            next_id: Arc::new(AtomicI64::new(1)),
            spec,
            versions: RefCell::new(HashMap::new()),
        };
        client.initialize(root)?;
        Ok(client)
    }

    fn initialize(&self, root: &std::path::Path) -> Result<()> {
        let id = self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": path_to_uri(root),
                "capabilities": {
                    "textDocument": {
                        "hover":          { "contentFormat": ["plaintext", "markdown"] },
                        "definition":     {},
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                        "publishDiagnostics": {},
                        "synchronization": { "didSave": true },
                    }
                },
                "clientInfo": { "name": "dxdiary" },
            }),
        )?;
        let _ = id;
        self.notify("initialized", json!({}))
    }

    /// Send a request. The reply arrives on `events` as [`Event::Response`].
    pub fn request(&self, method: &str, params: Value) -> Result<i64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.outgoing
            .send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .context("server is gone")?;
        Ok(id)
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.outgoing
            .send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .context("server is gone")?;
        Ok(())
    }

    /// Tell the server about a file we are showing.
    ///
    /// Opening a document the server already has is a protocol error, so a
    /// second call for the same path is a no-op rather than a duplicate.
    pub fn did_open(&self, path: &std::path::Path, language_id: &str, text: &str) -> Result<()> {
        let uri = path_to_uri(path);
        if self.versions.borrow().contains_key(&uri) {
            return Ok(());
        }
        self.versions.borrow_mut().insert(uri.clone(), 1);
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text,
            }}),
        )
    }

    /// Send the document's new full text.
    ///
    /// Always the whole document, never a range. The specification makes the
    /// range-less form valid whatever sync kind the server announced, and
    /// every server dxdiary ships a spec for accepts it. Incremental sync
    /// would save bandwidth on a pipe to a local process, which is nothing,
    /// at the cost of an edit log that must exactly mirror the rope — a class
    /// of bug that is silent until diagnostics land on the wrong line.
    ///
    /// Returns the version sent, or `None` when the document was never
    /// opened -- a change for an unknown document is dropped, not sent, since
    /// the server would reject it.
    pub fn did_change(&self, path: &std::path::Path, text: &str) -> Result<Option<i32>> {
        let uri = path_to_uri(path);
        let version = {
            let mut versions = self.versions.borrow_mut();
            let Some(v) = versions.get_mut(&uri) else {
                return Ok(None);
            };
            *v += 1;
            *v
        };
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ { "text": text } ],
            }),
        )?;
        Ok(Some(version))
    }

    /// The document was written to disk. Servers that build (gopls, clangd
    /// with a compilation database) use this as their cue.
    pub fn did_save(&self, path: &std::path::Path) -> Result<()> {
        let uri = path_to_uri(path);
        if !self.versions.borrow().contains_key(&uri) {
            return Ok(());
        }
        self.notify(
            "textDocument/didSave",
            json!({"textDocument": { "uri": uri }}),
        )
    }

    /// The document is no longer shown; the server owns its truth again.
    pub fn did_close(&self, path: &std::path::Path) -> Result<()> {
        let uri = path_to_uri(path);
        if self.versions.borrow_mut().remove(&uri).is_none() {
            return Ok(());
        }
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": { "uri": uri }}),
        )
    }

    /// Current version of an open document, for tests.
    pub fn version_of(&self, path: &std::path::Path) -> Option<i32> {
        self.versions.borrow().get(&path_to_uri(path)).copied()
    }

    pub fn hover(&self, path: &std::path::Path, line: u32, character: u32) -> Result<i64> {
        self.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": path_to_uri(path) },
                "position": { "line": line, "character": character },
            }),
        )
    }

    pub fn definition(&self, path: &std::path::Path, line: u32, character: u32) -> Result<i64> {
        self.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": path_to_uri(path) },
                "position": { "line": line, "character": character },
            }),
        )
    }

    /// Ask the server to exit, then reap it.
    pub fn shutdown(&mut self) {
        let _ = self.request("shutdown", json!(null));
        let _ = self.notify("exit", json!(null));
        // Do not wait indefinitely on a server that ignores `exit`.
        for _ in 0..50 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Classify one incoming message.
///
/// Requests *from* the server (those with both `id` and `method`) are ignored:
/// dxdiary advertises no capabilities that would provoke one.
pub fn to_event(msg: &Value) -> Option<Event> {
    let has_method = msg.get("method").is_some();
    let id = msg.get("id").and_then(Value::as_i64);

    if has_method {
        if id.is_some() {
            return None; // server-to-client request
        }
        let method = msg["method"].as_str()?;
        if method == "textDocument/publishDiagnostics" {
            let params = msg.get("params")?;
            let uri = params.get("uri")?.as_str()?.to_string();
            let items = params
                .get("diagnostics")
                .and_then(|d| serde_json::from_value(d.clone()).ok())
                .unwrap_or_default();
            return Some(Event::Diagnostics { uri, items });
        }
        return None; // other notifications: progress, logs, telemetry
    }

    let id = id?;
    if let Some(error) = msg.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string();
        return Some(Event::Error { id, message });
    }
    Some(Event::Response {
        id,
        result: msg.get("result").cloned().unwrap_or(Value::Null),
    })
}

/// `file://` URI for a path.
///
/// Percent-encodes the characters that actually appear in source trees; a full
/// URI crate would be a dependency for one function.
pub fn path_to_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' | ':' => out.push(ch),
            _ => {
                let mut buf = [0u8; 4];
                for b in ch.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

/// Turn a `file://` URI from a server back into a path.
///
/// The inverse of [`path_to_uri`]. Servers answer `definition` with URIs they
/// built themselves, so this has to cope with any percent-encoding and not
/// only the set `path_to_uri` emits. Returns `None` for a non-`file` scheme --
/// a server may name a location inside a jar or a generated buffer, and there
/// is nothing on disk to open.
pub fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // An authority is allowed but always empty for local files, so anything
    // before the first `/` is dropped rather than guessed at.
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(i) => &rest[i..],
        None => return None,
    };

    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            // A stray `%` falls through and is kept, rather than failing the
            // whole URI.
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    let decoded = String::from_utf8(out).ok()?;
    Some(std::path::PathBuf::from(decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn diagnostics_notifications_become_events() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/a.c",
                "diagnostics": [{
                    "range": {"start": {"line": 1, "character": 0},
                              "end":   {"line": 1, "character": 5}},
                    "message": "boom"
                }]
            }
        });
        match to_event(&msg).expect("an event") {
            Event::Diagnostics { uri, items } => {
                assert_eq!(uri, "file:///tmp/a.c");
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].message, "boom");
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn replies_become_responses() {
        let msg = json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
        match to_event(&msg).unwrap() {
            Event::Response { id, result } => {
                assert_eq!(id, 7);
                assert_eq!(result["ok"], true);
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn error_replies_are_distinguished_from_results() {
        let msg = json!({"jsonrpc": "2.0", "id": 3,
                         "error": {"code": -32601, "message": "method not found"}});
        match to_event(&msg).unwrap() {
            Event::Error { id, message } => {
                assert_eq!(id, 3);
                assert_eq!(message, "method not found");
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn a_result_of_null_is_still_a_response() {
        // Servers answer "no definition found" with a null result, which must
        // not be mistaken for a missing reply.
        let msg = json!({"jsonrpc": "2.0", "id": 9, "result": null});
        assert!(matches!(
            to_event(&msg),
            Some(Event::Response { id: 9, .. })
        ));
    }

    #[test]
    fn uninteresting_notifications_are_ignored() {
        for method in ["window/logMessage", "$/progress", "telemetry/event"] {
            let msg = json!({"jsonrpc": "2.0", "method": method, "params": {}});
            assert!(to_event(&msg).is_none(), "{method} should be ignored");
        }
    }

    #[test]
    fn server_to_client_requests_are_ignored() {
        // Has both id and method: a request *from* the server. Answering the
        // wrong shape would confuse it more than silence does.
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "workspace/configuration"});
        assert!(to_event(&msg).is_none());
    }

    #[test]
    fn plain_paths_become_file_uris() {
        assert_eq!(path_to_uri(Path::new("/tmp/a.rs")), "file:///tmp/a.rs");
    }

    #[test]
    fn spaces_and_non_ascii_in_paths_are_percent_encoded() {
        assert_eq!(
            path_to_uri(Path::new("/tmp/my project/a.rs")),
            "file:///tmp/my%20project/a.rs"
        );
        // A path a server would reject if sent raw.
        assert!(path_to_uri(Path::new("/tmp/café.rs")).ends_with("caf%C3%A9.rs"));
    }

    #[test]
    fn windows_style_separators_are_normalised() {
        assert_eq!(
            path_to_uri(Path::new(r"C:\code\a.rs")),
            "file://C:/code/a.rs"
        );
    }

    #[test]
    fn a_uri_round_trips_back_to_its_path() {
        for path in [
            "/tmp/a.rs",
            "/tmp/with space/b.rs",
            "/tmp/\u{3b1}\u{3b2}/c.rs",
            "/tmp/a+b/d.rs",
            "/tmp/100%/e.rs",
        ] {
            let uri = path_to_uri(Path::new(path));
            assert_eq!(
                uri_to_path(&uri).unwrap(),
                Path::new(path),
                "round trip failed for {path} via {uri}"
            );
        }
    }

    #[test]
    fn an_encoding_we_do_not_emit_is_still_decoded() {
        // Servers build their own URIs and encode more than we do.
        assert_eq!(
            uri_to_path("file:///tmp/a%2Fb/c%2Ers").unwrap(),
            Path::new("/tmp/a/b/c.rs")
        );
    }

    #[test]
    fn an_empty_authority_is_dropped() {
        assert_eq!(
            uri_to_path("file:///tmp/a.rs").unwrap(),
            Path::new("/tmp/a.rs")
        );
    }

    #[test]
    fn a_scheme_with_nothing_on_disk_is_refused() {
        // A server may name a location inside an archive or a virtual buffer.
        assert_eq!(uri_to_path("jar:file:///x.jar!/A.java"), None);
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
    }

    #[test]
    fn a_truncated_escape_does_not_lose_the_rest_of_the_path() {
        assert_eq!(
            uri_to_path("file:///tmp/a%").unwrap(),
            Path::new("/tmp/a%"),
            "a stray percent is kept rather than failing the URI"
        );
    }
}
