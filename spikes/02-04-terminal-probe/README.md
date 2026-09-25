# Spike 0.2 / 0.4 — terminal probe

> **Both spikes now pass, automatically.** Run `./run-herdr.sh` — **18/18**, no human
> with a mouse required. It drives two chains headlessly and injects SGR mouse reports
> with `wezterm cli send-text`, because a mouse report is just bytes on stdin:
>
> ```
> wezterm →        herdr → probe
> wezterm → ssh →  herdr → probe
> ```
>
> The SSH variant starts a throwaway sshd as the current user on port 2222 — no root,
> and it never touches `~/.ssh/authorized_keys`. Pass `local` to skip it.
> Results in [`../FINDINGS.md`](../FINDINGS.md).
>
> The manual instructions below remain useful for terminals the harness cannot drive.

---

Answers the two blocking questions from `DESIGN.md` §8 that can only be answered in
the real environment:

- **0.2** Does herdr forward mouse events into a plugin pane, with coordinates intact
  — including past column 223, where the legacy X10 encoding breaks and SGR (1006)
  is required?
- **0.4** Does rendering hold up — truecolor, unicode widths, box drawing, resize —
  under herdr, and over SSH?

If 0.2 fails, requirement 4 (clickable UI) does not work inside herdr and needs
escalating upstream before any of Phase 1 is written.

## Build

```bash
cargo build --release
```

Pure Rust — ratatui and crossterm only. No C toolchain needed, unlike the
tree-sitter spike.

## Run it three times

The whole point is the comparison. Same terminal, same window size, three contexts:

```bash
./target/release/spike-terminal-probe
```

1. **Bare terminal** — baseline. Establishes what your terminal can do.
2. **Inside a herdr pane** — isolates herdr's multiplexing. Any capability that
   worked in (1) and fails here is herdr eating it.
3. **Over SSH, inside herdr** — isolates the wire. Run it on the office box from
   home, or vice versa.

**Widen the terminal past 223 columns before running**, or the SGR check is skipped —
and that check is the one that matters most for a code pane, since any real diff view
exceeds column 223 on a wide monitor.

## What to do in it

Exercise every input path, then press `q`:

- move the mouse around
- left-click each of the four targets, especially **far RIGHT**
- right-click somewhere
- scroll up and down
- drag (press, move, release)
- resize the terminal window

## Reading the result

The TUI shows live state; the verdict prints to stdout on exit. Paste all three
verdicts into `spikes/FINDINGS.md`.

Things to look at while it's running:

| Panel | What you're checking |
|---|---|
| environment | `herdr pane: YES` confirms `HERDR_ENV` reaches the pane. `COLORTERM=truecolor` in green means 24-bit is advertised. |
| colour | The top gradient must be **smooth**. Visible banding means you're silently on 256 colours — the theme design in Phase 3 has to account for it. |
| unicode | The CJK line and the ASCII line below it must end at the **same column**. If they don't, wide-character width handling is broken and the diff gutter will misalign. |
| click targets | Each turns green when clicked. `max mouse col` above must exceed 223 for the SGR check to pass. |
| event log | Coordinates should track the pointer exactly. Offsets here mean herdr is inserting chrome the pane doesn't know about. |

## Failure modes worth naming

- **No mouse events at all in herdr** — herdr is capturing mouse for its own pane
  management and not forwarding. Check whether herdr has a config option to pass
  mouse through to plugin panes; if not, this is an upstream ask.
- **Events arrive but coordinates are offset** — herdr is not accounting for pane
  origin. Workable: dxdiary can correct with a fixed offset, but it needs detecting.
- **Coordinates freeze or wrap around column 223** — X10 encoding. crossterm requests
  SGR, so this would mean something in the chain is downgrading it. Fatal for wide
  panes.
- **Works locally, fails over SSH** — the wire is stripping the mouse reporting mode.
  Check `TERM` on the remote side.
