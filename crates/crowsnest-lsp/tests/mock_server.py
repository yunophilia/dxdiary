#!/usr/bin/env python3
"""A minimal LSP server, for exercising the client end to end.

No real language server is guaranteed to be installed on any machine that
builds crowsnest, so the protocol client would otherwise only ever be tested by
hand -- which is how protocol clients rot. This speaks just enough LSP to prove
the handshake, request/response correlation, notifications, and error replies
all work over real pipes.

Behaviour is deliberately simple and deterministic:

  initialize            -> capabilities
  textDocument/didOpen  -> publishDiagnostics with one error on line 1
  textDocument/hover    -> hover text naming the position
  textDocument/definition -> a location in the same file
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
            send({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {
                    "uri": uri,
                    "diagnostics": [{
                        "range": {"start": {"line": 1, "character": 0},
                                  "end": {"line": 1, "character": 4}},
                        "severity": 1,
                        "message": "mock diagnostic",
                    }],
                },
            })
        elif method == "textDocument/hover":
            pos = msg["params"]["position"]
            reply(msg_id, {"contents": {
                "kind": "plaintext",
                "value": "hover at %d:%d" % (pos["line"], pos["character"]),
            }})
        elif method == "textDocument/definition":
            uri = msg["params"]["textDocument"]["uri"]
            reply(msg_id, {"uri": uri,
                           "range": {"start": {"line": 0, "character": 0},
                                     "end": {"line": 0, "character": 1}}})
        elif method == "shutdown":
            reply(msg_id, None)
        elif method == "exit":
            return 0
        elif msg_id is not None:
            send({"jsonrpc": "2.0", "id": msg_id,
                  "error": {"code": -32601, "message": "method not found"}})


if __name__ == "__main__":
    sys.exit(main())
