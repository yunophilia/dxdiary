# Spike 05 — automated WezTerm testing

Capability detection cannot be unit-tested: it is a conversation between the program
and a real terminal emulator. This harness makes that conversation scriptable.

`wezterm-mux-server` is WezTerm's headless multiplexer — real terminal emulation, no
GUI, no X display. `wezterm cli` then spawns panes, types into them, and reads back
what is on screen. That is enough to test the real code path in CI.

```bash
./run.sh [path-to-crowsnest]
```

## Setup

Everything installs into `~/tools` with no `sudo`.

```bash
curl -sSL "$(curl -sS https://api.github.com/repos/wezterm/wezterm/releases/latest \
  | grep -o 'https://[^"]*\.AppImage' | head -1)" -o ~/tools/wezterm.AppImage
```

Then `chmod +x` it and run `./wezterm.AppImage --appimage-extract`; the binaries land
in `squashfs-root/usr/bin`. Extraction avoids needing FUSE.

For zsh without root, [zsh-bin](https://github.com/romkatv/zsh-bin) ships static
builds that need no ncurses headers:

```bash
curl -sSL "$(curl -sS https://api.github.com/repos/romkatv/zsh-bin/releases/latest \
  | grep -o 'https://[^"]*linux-x86_64\.tar\.gz' | head -1)" -o ~/tools/zsh.tar.gz
```

## What it checks

Per shell (bash and zsh):

- truecolor is detected
- the answer came from **XTGETTCAP**, not a guess
- the terminal is identified via XTVERSION
- **`TERM=dumb COLORTERM=` still yields truecolor** — the point of querying rather
  than sniffing is that a wrong `TERM` cannot downgrade a capable terminal

Plus a no-tty control: piped output must fall back to the environment and must not
leak query escape sequences into stdout.

## Findings

**WezTerm 20240203 replies:**

| query | reply |
|---|---|
| `DCS +q 524742 ST` — XTGETTCAP `RGB` | `1+r524742=382F382F38` → `"8/8/8"` |
| `CSI > q` — XTVERSION | `WezTerm 20240203-110809-5046fc22` |
| `CSI c` — DA1 | `?65;4;6;18;22c` |
| `CSI ? u` — kitty keyboard | **no reply** |

Two of these overturned assumptions the design had been built on:

**WezTerm does set `COLORTERM`.** Inside a pane: `COLORTERM=truecolor`,
`TERM_PROGRAM=WezTerm`, `WEZTERM_EXECUTABLE` set. [wezterm#875](https://github.com/wezterm/wezterm/issues/875)
was fixed; the issue title is still the top search result and reading it as current
behaviour was wrong.

**WezTerm does not answer the kitty keyboard query.** Colour detection had been built
on "supports kitty keyboard ⇒ truecolor", which never fires here — the main target
would have fallen through to guessing. XTGETTCAP, dismissed earlier as too risky to
hand-roll, is the signal that actually works.

The stdin-race concern behind that dismissal was real but solvable: query
synchronously on the main thread before the event loop starts, and use DA1 as a sync
marker so the read ends when replies arrive rather than when a timeout expires.

**The shell is not a variable.** bash and zsh give identical results, which is the
expected outcome — the query is between the program and the terminal emulator, and
the shell only execs the binary. Worth having tested rather than assumed.

`raw_query.py` sends the queries directly and dumps raw replies. Reach for it when a
terminal misbehaves: it distinguishes "the terminal does not support this" from "our
parsing is wrong", which a bare `false` from a detection helper cannot.

## Caveat

This is `wezterm-mux-server`, not `wezterm-gui`. The terminal emulation is shared, so
replies should match, but the kitty-keyboard negative in particular is worth
re-confirming against the GUI before treating it as WezTerm-wide. It does not change
the design either way — XTGETTCAP is the primary path and it works in both.
