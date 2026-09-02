#!/usr/bin/env python3
"""Send terminal capability queries and report the raw replies.

Distinguishes "the terminal does not support this" from "our detection is
broken" -- crossterm reports both as a plain false.

Queries sent, in order:
  CSI ? u   kitty keyboard protocol: report current flags
  CSI > q   XTVERSION: terminal name and version
  DCS +q 524742 ST   XTGETTCAP for the `RGB` terminfo capability
  CSI c     DA1, last so its reply marks the end of the batch
"""

import os
import select
import sys
import termios
import tty

QUERIES = [
    ("kitty-keyboard  CSI ? u", "\x1b[?u"),
    ("xtversion       CSI > q", "\x1b[>q"),
    ("xtgettcap RGB   DCS +q", "\x1bP+q524742\x1b\\"),
    ("da1             CSI c", "\x1b[c"),
]


def main() -> int:
    fd = sys.stdin.fileno()
    if not os.isatty(fd):
        print("not a tty")
        return 1

    saved = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        sys.stdout.write("".join(q for _, q in QUERIES))
        sys.stdout.flush()

        chunks = []
        # DA1 is last, so once it replies everything prior has arrived.
        while select.select([fd], [], [], 1.0)[0]:
            data = os.read(fd, 4096)
            if not data:
                break
            chunks.append(data)
            if b"\x1b[?" in b"".join(chunks) and b"c" in data[-4:]:
                break
        raw = b"".join(chunks)
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)

    print("queries sent:")
    for name, q in QUERIES:
        print(f"  {name:<24} {q.encode()!r}")
    print()
    print(f"raw reply ({len(raw)} bytes): {raw!r}")
    print()

    replies = [r for r in raw.split(b"\x1b") if r]
    for r in replies:
        print(f"  reply: {b'ESC' + r!r}")
    print()

    kitty = raw.startswith(b"\x1b[?") and b"u" in raw.split(b"\x1b[?")[1][:8]
    print(f"kitty keyboard protocol : {'YES' if kitty else 'NO'}")
    print(f"xtversion replied       : {'YES' if b'>|' in raw else 'NO'}")
    print(f"xtgettcap RGB replied   : {'YES' if b'+r' in raw else 'NO'}")
    print(f"da1 replied             : {'YES' if b'?' in raw and raw.rstrip().endswith(b'c') else 'NO'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
