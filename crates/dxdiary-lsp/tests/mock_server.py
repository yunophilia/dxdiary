#!/usr/bin/env python3
"""A minimal LSP server, for exercising the client end to end.

No real language server is guaranteed to be installed on any machine that
builds dxdiary, so the protocol client would otherwise only ever be tested by
hand -- which is how protocol clients rot. This speaks just enough LSP to prove
the handshake, request/response correlation, notifications, and error replies
all work over real pipes.

Behaviour is deliberately simple and deterministic:

  initialize            -> capabilities
  textDocument/didOpen  -> publishDiagnostics with one error on line 1
  textDocument/didChange -> publishDiagnostics on the document's last line,
                           message naming the version received, so a test
                           can prove which text the server is looking at
  textDocument/didSave  -> publishDiagnostics with message "saved"
  textDocument/didClose -> publishDiagnostics with an empty list
  textDocument/hover    -> hover text naming the position
  textDocument/definition -> a location in a sibling file, defined.rs:3
  anything else with an id -> a JSON-RPC "method not found" error
  shutdown / exit       -> reply, then exit
"""

import json
import sys


def read_message():
    """Read one Content-Length framed message, or None at EOF."""
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode("utf-8", "replace").strip()
        if not line:
            break
        name, _, value = line.partition(":")
        if name.strip().lower() == "content-length":
            length = int(value.strip())
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))


def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def diagnostic(line, message):
    return {
        "range": {"start": {"line": line, "character": 0},
                  "end": {"line": line, "character": 4}},
        "severity": 1,
        "message": message,
    }


def publish(uri, diagnostics):
    send({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": diagnostics},
    })


def reply(msg_id, result):
    send({"jsonrpc": "2.0", "id": msg_id, "result": result})


def main():
    while True:
        msg = read_message()
        if msg is None:
            return 0

        method = msg.get("method")
        msg_id = msg.get("id")

        if method == "initialize":
            reply(msg_id, {"capabilities": {"hoverProvider": True,
                                            "definitionProvider": True}})
        elif method == "initialized":
            pass
        elif method == "textDocument/didOpen":
            uri = msg["params"]["textDocument"]["uri"]
            publish(uri, [diagnostic(1, "mock diagnostic")])
        elif method == "textDocument/didChange":
            doc = msg["params"]["textDocument"]
            changes = msg["params"]["contentChanges"]
            # Full-content sync: exactly one change with no range.
            assert len(changes) == 1 and "range" not in changes[0], changes
            last = changes[0]["text"].count("\n")
            publish(doc["uri"], [diagnostic(last, "v%d" % doc["version"])])
        elif method == "textDocument/didSave":
            publish(msg["params"]["textDocument"]["uri"], [diagnostic(0, "saved")])
        elif method == "textDocument/didClose":
            publish(msg["params"]["textDocument"]["uri"], [])
        elif method == "textDocument/hover":
            pos = msg["params"]["position"]
            reply(msg_id, {"contents": {
                "kind": "plaintext",
                "value": "hover at %d:%d" % (pos["line"], pos["character"]),
            }})
        elif method == "textDocument/definition":
            # A sibling file, so a client's cross-file jump is exercised.
            uri = msg["params"]["textDocument"]["uri"]
            target = uri.rsplit("/", 1)[0] + "/defined.rs"
            reply(msg_id, {"uri": target,
                           "range": {"start": {"line": 2, "character": 4},
                                     "end": {"line": 2, "character": 8}}})
        elif method == "shutdown":
            reply(msg_id, None)
        elif method == "exit":
            return 0
        elif msg_id is not None:
            send({"jsonrpc": "2.0", "id": msg_id,
                  "error": {"code": -32601, "message": "method not found"}})


if __name__ == "__main__":
    sys.exit(main())
