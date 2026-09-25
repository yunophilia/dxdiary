//! End-to-end client tests against `mock_server.py`.
//!
//! Real pipes, a real child process, real framing — everything except a real
//! language server, none of which is guaranteed to be installed. Skipped
//! entirely if `python3` is unavailable rather than failing the build.

use std::process::Command;
use std::time::{Duration, Instant};

use dxdiary_lsp::client::{Client, Event};
use dxdiary_lsp::registry::{spec_for, ServerSpec};
use dxdiary_syntax::Language;

fn python() -> Option<&'static str> {
    ["python3", "python"]
        .into_iter()
        .find(|c| dxdiary_lsp::registry::find_on_path(c).is_some())
}

fn mock_client() -> Option<(Client, tempdir::Dir)> {
    let python = python()?;
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("mock_server.py");

    let dir = tempdir::Dir::new("dxdiary-lsp");
    let mut cmd = Command::new(python);
    cmd.arg(&script);

    let spec: &'static ServerSpec = spec_for(Language::Rust).unwrap();
    let client = Client::from_command(cmd, spec, dir.path()).expect("mock server starts");
    Some((client, dir))
}

/// Wait for an event matching `want`, ignoring the rest.
fn wait_for<F>(client: &Client, timeout: Duration, mut want: F) -> Option<Event>
where
    F: FnMut(&Event) -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match client.events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                if want(&event) {
                    return Some(event);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return None,
        }
    }
    None
}

const TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn the_handshake_completes_and_a_hover_round_trips() {
    let Some((client, dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };

    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let id = client.hover(&file, 3, 7).expect("request sent");
    let event = wait_for(
        &client,
        TIMEOUT,
        |e| matches!(e, Event::Response { id: got, .. } if *got == id),
    )
    .expect("a reply for our request");

    match event {
        Event::Response { result, .. } => {
            let text = result["contents"]["value"].as_str().unwrap_or_default();
            assert_eq!(text, "hover at 3:7", "the position round-tripped");
        }
        other => panic!("wrong event: {other:?}"),
    }
}

#[test]
fn opening_a_file_delivers_diagnostics() {
    let Some((client, dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };

    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn main() {}\nlet x =\n").unwrap();
    client
        .did_open(&file, "rust", "fn main() {}\nlet x =\n")
        .unwrap();

    let event = wait_for(&client, TIMEOUT, |e| matches!(e, Event::Diagnostics { .. }))
        .expect("diagnostics arrive");

    match event {
        Event::Diagnostics { uri, items } => {
            assert!(uri.ends_with("a.rs"), "{uri}");
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].message, "mock diagnostic");
            assert_eq!(items[0].range.start.line, 1);
        }
        other => panic!("wrong event: {other:?}"),
    }
}

#[test]
fn responses_are_matched_to_the_request_that_asked() {
    let Some((client, dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    // Two requests in flight at once: ids must not be crossed.
    let first = client.hover(&file, 1, 1).unwrap();
    let second = client.hover(&file, 2, 2).unwrap();
    assert_ne!(first, second, "ids are unique");

    // The `initialize` reply arrives on this same channel, so a caller must
    // match on its own id rather than taking whatever turns up next.
    let mut seen = std::collections::HashMap::new();
    let deadline = Instant::now() + TIMEOUT;
    while seen.len() < 2 && Instant::now() < deadline {
        if let Ok(Event::Response { id, result }) =
            client.events.recv_timeout(Duration::from_millis(100))
        {
            if id != first && id != second {
                continue;
            }
            let text = result["contents"]["value"]
                .as_str()
                .unwrap_or_else(|| panic!("hover {id} had no contents: {result}"));
            seen.insert(id, text.to_string());
        }
    }

    assert_eq!(seen.get(&first).map(String::as_str), Some("hover at 1:1"));
    assert_eq!(seen.get(&second).map(String::as_str), Some("hover at 2:2"));
}

#[test]
fn an_unsupported_method_comes_back_as_an_error_not_a_hang() {
    let Some((client, _dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };

    let id = client
        .request("textDocument/nonsense", serde_json::json!({}))
        .unwrap();
    let event = wait_for(
        &client,
        TIMEOUT,
        |e| matches!(e, Event::Error { id: got, .. } if *got == id),
    )
    .expect("an error reply");

    match event {
        Event::Error { message, .. } => assert!(message.contains("not found"), "{message}"),
        other => panic!("wrong event: {other:?}"),
    }
}

#[test]
fn definition_returns_a_location() {
    let Some((client, dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let id = client.definition(&file, 0, 3).unwrap();
    let event = wait_for(
        &client,
        TIMEOUT,
        |e| matches!(e, Event::Response { id: got, .. } if *got == id),
    )
    .expect("a reply");

    match event {
        Event::Response { result, .. } => {
            assert!(result["uri"].as_str().unwrap().ends_with("a.rs"));
        }
        other => panic!("wrong event: {other:?}"),
    }
}

#[test]
fn a_server_that_exits_is_reported_rather_than_hanging() {
    let Some((mut client, _dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };

    client.shutdown();
    let closed = wait_for(&client, TIMEOUT, |e| matches!(e, Event::Closed(_)));
    assert!(closed.is_some(), "the reader thread reports the close");
}

/// Minimal scoped temp directory; the `tempfile` crate would be a dependency
/// for six lines.
mod tempdir {
    use std::path::{Path, PathBuf};

    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new(tag: &str) -> Self {
            let unique = format!(
                "{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            );
            let path = std::env::temp_dir().join(unique.replace(['(', ')', ' '], ""));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Dir(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[test]
fn changes_carry_increasing_versions_and_the_full_text() {
    let Some((client, dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };
    let file = dir.path().join("a.rs");
    client.did_open(&file, "rust", "fn main() {}\n").unwrap();
    assert_eq!(client.version_of(&file), Some(1));

    // The mock publishes a diagnostic on the last line of whatever text it was
    // sent, with the version in the message: proof of both what arrived and
    // which version it was labelled with.
    assert_eq!(
        client
            .did_change(&file, "fn main() {}\n\n\nlet x =\n")
            .unwrap(),
        Some(2)
    );
    let event = wait_for(&client, TIMEOUT, |e| {
        matches!(e, Event::Diagnostics { items, .. } if items.iter().any(|d| d.message == "v2"))
    })
    .expect("diagnostics for version 2 arrive");
    let Event::Diagnostics { items, .. } = event else {
        unreachable!()
    };
    assert_eq!(items[0].range.start.line, 4, "last line of the new text");

    assert_eq!(client.did_change(&file, "").unwrap(), Some(3));
    assert_eq!(client.version_of(&file), Some(3));
}

#[test]
fn a_change_to_a_document_never_opened_is_dropped() {
    let Some((client, dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };
    let file = dir.path().join("never.rs");
    assert_eq!(client.did_change(&file, "x").unwrap(), None);
    assert_eq!(client.version_of(&file), None);
}

#[test]
fn opening_twice_does_not_reset_the_version() {
    let Some((client, dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };
    let file = dir.path().join("a.rs");
    client.did_open(&file, "rust", "a").unwrap();
    client.did_change(&file, "ab").unwrap();
    client.did_open(&file, "rust", "ab").unwrap();
    assert_eq!(client.version_of(&file), Some(2), "second open is a no-op");
}

#[test]
fn save_and_close_reach_the_server() {
    let Some((client, dir)) = mock_client() else {
        eprintln!("skipped: python3 not available");
        return;
    };
    let file = dir.path().join("a.rs");
    client.did_open(&file, "rust", "a").unwrap();

    client.did_save(&file).unwrap();
    wait_for(&client, TIMEOUT, |e| {
        matches!(e, Event::Diagnostics { items, .. } if items.iter().any(|d| d.message == "saved"))
    })
    .expect("didSave observed");

    client.did_close(&file).unwrap();
    wait_for(
        &client,
        TIMEOUT,
        |e| matches!(e, Event::Diagnostics { items, .. } if items.is_empty()),
    )
    .expect("didClose clears diagnostics");
    assert_eq!(client.version_of(&file), None);

    // Closed means forgotten: a change now goes nowhere.
    assert_eq!(client.did_change(&file, "b").unwrap(), None);
}
