# Phase 0 — Spike findings

Environments — both spikes were built and run on each, with identical results:

| | Linux | Windows |
|---|---|---|
| host | WSL Ubuntu 20.04 | Windows 11 |
| toolchain | `stable-x86_64-unknown-linux-gnu` | `stable-x86_64-pc-windows-gnu` |
| rustc | 1.98.0 | 1.98.0 |
| C compiler | gcc 9.4.0 | MinGW-w64 gcc 16.1.0 (WinLibs MSVCRT) |
| also present | clang 10.0.0 | — |

Crates: tree-sitter 0.26.12, tree-sitter-c 0.24.2, gix 0.86.0, ratatui 0.30.2,
crossterm 0.29.0.

**Cross-platform build is confirmed working**, including the C-compiling path —
tree-sitter grammars build and run natively on Windows via MinGW, not just on Linux.

---

## Spike 0.1 — tree-sitter-c vs GCC nested functions — **PASS**

**Question:** Can tree-sitter be the authority for C highlighting, outline, and
same-file navigation, given clangd cannot parse GCC nested functions?

**Answer: yes.** tree-sitter-c parses nested functions cleanly.

```
[1] Parse integrity
    root.has_error() = false
    ✓ no ERROR / MISSING nodes

[2] Symbol outline
    L7   fn outer
    L12      fn helper       <NESTED>
    L18      fn accumulate   <NESTED>
    L22          fn bump     <NESTED>   (nested 2 deep)
    L40  struct after_marker
    L45  fn after_nested
    L50  typedef after_kind
    L52  fn main

[3] ✓ 3 nested functions detected: helper(1), accumulate(1), bump(2)
[4] ✓ all post-nested-function symbols found correctly
```

The grammar allows this by design — `_block_item` includes `$.function_definition`
directly, so `compound_statement` accepts nested definitions. Nesting depth is
recoverable by counting enclosing `function_definition` ancestors, which is exactly
the detector §2's diagnostic filter needs.

### The awkward corners hold up too

`nested.c` only covers a function inside a function. A second fixture,
`nested_hard.c`, exercises the parts of the extension that actually look like they
might break a grammar:

- `auto int second(int);` — forward-declaring a nested function so two nested
  functions can call each other
- `__label__ bail;` — a local label, so a nested function can `goto` out of the
  enclosing function entirely
- taking the address of a nested function and passing it to `qsort` (GCC generates a
  trampoline)

tree-sitter parses all of it with **zero ERROR nodes** and correctly identifies all
four nested functions, including the mutually-recursive pair. Symbols after the
nested blocks are all intact.

**Verified against real compilers:**

| Compiler | nested.c | nested_hard.c | plain.c |
|---|---|---|---|
| gcc 9.4 `-std=gnu11` | compiles, runs (`60 5 x`) | compiles, runs (`3 -5 9 42`) | ok |
| clang 10 `-fsyntax-only` | **2 errors** | **6 errors** | exit 0 |
| tree-sitter-c 0.24.2 | clean | clean | clean |

### Correction to DESIGN.md §2 — blast radius is smaller than stated

The design doc claimed clang's parse recovery corrupts "everything after the nested
function in that translation unit." The measurement says otherwise:

```
nested.c:13:5: error: function definition is not allowed here
nested.c:19:5: error: function definition is not allowed here
nested.c:33:12: warning: implicit declaration of function 'accumulate'
```

Damage is **contained to the enclosing function**. `outer()` is wrecked — its nested
definitions are rejected and the call to `accumulate` at L33 degrades to an implicit
declaration. But `after_marker`, `after_nested`, `after_kind`, and `main` all
compiled clean.

This changes the filter design for the better: suppress clangd diagnostics **within
the enclosing function's byte range**, not from the nested function to end-of-file.
Much less clangd output gets discarded, so C keeps more real diagnostics than
originally planned.

**Caveat:** clang 10 (2020) is what was available. Re-verify against current clang
and an actual `clangd` before finalizing the filter — no clangd binary is installed,
and `sudo` prompts for a password here so apt install needs a human.

### Implementation note for Phase 3

The spike's outline extractor produced one false positive:

```
L54      struct after_marker  <NESTED>
```

That's `struct after_marker m = {...}` — a *use* of the type inside `main`, not a
definition. `struct_specifier` matches both. The real extractor must require a `body`
field to distinguish definition from reference. Same trap applies to
`enum_specifier` and `union_specifier`.

---

## Spike 0.3 — gix performance and API coverage — **PASS, with two caveats**

**API coverage: complete.** Every operation dxdiary needs exists in gix 0.86:
`repo.merge_base()`, `repo.diff_tree_to_tree()`, `repo.status().into_index_worktree_iter()`,
and `gix::blame::file()`. The spike compiled clean on the first try against the real
API. No gaps, no need for shelling out to `git`.

One ergonomic note: **blame is low-level.** Unlike git2's `repo.blame_file()`, gix
wants an odb handle, a suspect commit, a diff resource cache, and an options struct
assembled by hand. Budget an afternoon wrapping it in `dxdiary-vcs`.

### Perf: the filesystem dominated everything

First run, repo on `/mnt/e` (WSL's 9p bridge to Windows), vs the same repo copied to
the WSL-native ext4:

| Operation | on `/mnt/e` (9p) | native ext4 | speedup |
|---|---|---|---|
| open repository | 36.80 ms | 0.37 ms | 99× |
| merge_base | 5.65 ms | 2.99 ms | 1.9× |
| diff merge_base..HEAD | 162.39 ms | **0.63 ms** | 258× |
| status (index↔worktree) | 689.08 ms | 122.61 ms | 5.6× |

*(Gamma-Music-Manager, 1122 commits, 183 changed paths.)*

**This is a design-level finding, not a benchmarking artifact.** Any cross-filesystem
access — Windows dxdiary against a WSL path, or the reverse — collapses
performance. It independently justifies the SSH-first architecture in §1: run
dxdiary *where the code lives*, never across a filesystem bridge.

### Scaling: git/git, 81,966 commits, 315 MB

| Operation | fork point HEAD~500 | HEAD~5000 |
|---|---|---|
| merge_base | 122 ms | **350 ms — OVER** |
| diff (1410 / 4623 paths) | 42.8 ms | 37.4 ms | 
| status | 77.9 ms | 13.6 ms |
| blame `builtin/rebase.c` (1931 lines) | 892 ms | — |
| blame `diff.c` (7858 lines, 3160 hunks) | — | **2339 ms — OVER** |

Diff is essentially flat regardless of history depth or change count — it only walks
two trees. Status is fine. The two cliffs are merge-base and blame.

### Mitigation tested: write a commit-graph

The repo had no commit-graph. Writing one took **0.93 s** and produced a 4.6 MB file:

| | before | after | |
|---|---|---|---|
| merge_base @ HEAD~5000 | 350 ms | **52.6 ms** | **6.7× faster** |
| merge_base @ HEAD~500 | 122 ms | 68.0 ms | 1.8× |
| blame `rebase.c` | 892 ms | **543 ms** | 1.6× |
| blame `diff.c` | 2339 ms | 2467 ms | no help |

**Action for the design:** dxdiary should ensure a commit-graph exists — check on
repo open, offer to write it in the background. Sub-second one-time cost, and it
takes merge-base comfortably inside budget on an 80k-commit repo. Blame benefits too,
since it walks commits.

**Blame remains the one unfixable-by-caching bottleneck.** 2.5 s on a 7858-line file
with deep history, commit-graph or not. This confirms the §5 decision: blame must be
computed off the render thread and streamed in progressively. Treat the 1 s budget as
"first visible hunks", not "complete result".

## Spike 05 — automated WezTerm testing — **PASS, and it overturned two assumptions**

Full detail in [`05-wezterm-harness/README.md`](05-wezterm-harness/README.md).

`wezterm-mux-server` runs WezTerm's terminal emulation headlessly, and `wezterm cli`
can spawn panes, type into them, and scrape the screen. Capability detection is
therefore testable in CI, which it previously was not.

**12/12 assertions pass** across bash and zsh, against a real WezTerm.

Two findings reversed the design:

1. **WezTerm *does* set `COLORTERM=truecolor`** (plus `TERM_PROGRAM=WezTerm` and
   `WEZTERM_EXECUTABLE`). DESIGN.md had claimed otherwise, citing
   [wezterm#875](https://github.com/wezterm/wezterm/issues/875) — an issue that was
   since fixed. Treating a search-result issue title as current behaviour was the
   mistake.

2. **WezTerm does *not* answer the kitty keyboard query** (`CSI ? u`). Colour
   detection had been built on "supports kitty keyboard ⇒ truecolor". On the primary
   target that inference never fires, so it silently fell through to guessing.

What does work is **XTGETTCAP**, which the earlier design dismissed as too risky:

```
→ DCS +q 524742 ST          (query terminfo `RGB`)
← 1+r524742=382F382F38      ("8/8/8" — eight bits per channel)
```

The risk behind that dismissal — a bespoke stdin reader racing the event loop and
eating keystrokes — was real but solvable. Query synchronously on the main thread
before the loop starts, and send DA1 last as a sync marker so the read ends when
replies arrive rather than when a timeout expires.

The strongest demonstration is a harness assertion: with `TERM=dumb COLORTERM=`,
detection **still reports truecolor**, because the terminal is answering for itself.
No amount of environment sniffing survives that.

bash and zsh produce identical results, as expected — the shell only execs the
binary; the conversation is with the terminal emulator.

## Spikes 0.2 + 0.4 — mouse and rendering under herdr — **PASS**

The last **High** risk in the design, and it clears. Requirement 4 (clickable UI)
works inside herdr.

Run with `spikes/02-04-terminal-probe/run-herdr.sh` — **18/18, stable across repeated
runs, including over a real SSH hop.** No human with a mouse required.

### The technique

These were "needs a person to click things" until spike 05 supplied the missing
piece: an SGR mouse report is just bytes on stdin, so `wezterm cli send-text` can
inject one.

```
ESC [ < button ; col ; row M      press    (col/row 1-based)
ESC [ < button ; col ; row m      release
```

Every layer is real — `wezterm-mux-server` does genuine terminal emulation, herdr
0.8.2 is the actual binary, the probe is an ordinary crossterm client, and the SSH
hop is a real sshd. Two chains are tested:

```
wezterm →        herdr → probe
wezterm → ssh →  herdr → probe
```

The SSH variant runs a throwaway sshd **as the current user on port 2222**, with its
own host key and `authorized_keys` under `~/.cache/spike-sshd`. No root, and it never
touches `~/.ssh/authorized_keys`. Localhost SSH is a real SSH hop — same channel, same
env handling — so it answers the question without needing a second machine.

A baseline run without herdr comes first: if injection does not work at all, nothing
downstream means anything.

### Results

| check | local | over SSH |
|---|---|---|
| herdr forwards mouse events into the pane | pass | pass |
| coordinates translated to pane-local | pass | pass |
| SGR survives past column 223 | pass — col 224 | pass — col 224 |
| `HERDR_ENV` reaches the pane | pass | pass |
| box-drawing renders | pass | pass |
| resize delivered on layout change | pass | pass |

Baseline without herdr also passes, including SGR at column 250.

**herdr translates coordinates, exactly.** herdr draws a 26-column sidebar, so its
pane starts at screen column 27. A click injected at screen column 250 arrived at the
probe as column 223 (0-based) — `250 - 27 = 223`, precise. Untranslated coordinates
would have landed on the wrong cell and the whole `HitMap` design would need an
offset correction; it does not.

Hit-testing was confirmed end to end by landing clicks on two specific targets in the
probe's UI through herdr — both registered.

**The column-223 case is the one that mattered.** Legacy X10 encoding adds 32 to each
coordinate and sends one byte, so it cannot express a 1-based column above 223. Any
real code pane on a wide monitor crosses that. Through herdr the click arrived at
1-based column 224, proving SGR (1006) is in use end to end.

0.4 comes along for free: truecolor is detected inside herdr, box-drawing and
unicode render correctly, and a resize event is delivered when herdr lays the pane
out at 234×29.

### SSH is transparent to mouse bytes, and hostile to environment variables

The SSH hop changes nothing about mouse forwarding — SSH is an 8-bit clean byte pipe,
so SGR reports cross it untouched, coordinates and all.

What it does destroy is the environment. Running `dxdiary --caps` through the hop:

```
colour depth       TrueColor
  source           XTGETTCAP — the terminal stated it
terminal           WezTerm 20240203-110809-5046fc22
TERM               xterm-256color
COLORTERM          (unset)
TERM_PROGRAM       (unset)
SSH                yes
```

**`COLORTERM` and `TERM_PROGRAM` are both gone**, because `ssh` forwards only `TERM`
unless `SendEnv`/`AcceptEnv` are configured. Environment sniffing would have concluded
`Indexed256` and silently downgraded every colour. XTGETTCAP queried straight through
the tunnel, got truecolor, and identified the terminal by name through it.

This is the scenario the §1 detection design was built for, now measured rather than
argued — and it is the strongest evidence for querying over sniffing.

One nuance: inside a **herdr** pane over SSH, `COLORTERM=truecolor` is present again,
because herdr sets it for the panes it spawns. So the raw-SSH case is the harsh one;
herdr repairs part of the damage for its children. Do not rely on that.

### Three harness bugs worth remembering

**`pkill -f herdr` killed the test.** The script is named `run-herdr.sh`, so `-f`
matched its own command line and SIGTERMed the run (exit 143). Use `pkill -x`.

**Fixed sleeps made it flaky.** The first version passed, then failed everything on
the next run. herdr's startup time varies, and a command sent into a pane that is not
ready is silently lost. Replaced with polling for readiness, plus a unique session
name per run — herdr sessions persist by name, so a fixed one has the second run
attach to leftover state.

**`pkill -x wezterm-mux-server` never matched anything.** `-x` matches the process
name, which the kernel truncates to 15 characters; the name is 18. A stale server
survived every cleanup and held the socket, so the next start failed with
`os error 11`. Use `-f` for long names, `-x` for short ones — and note the two flags
are needed for opposite reasons in the same script.

### Not covered

Only WezTerm. Whether other terminals behave the same under herdr is untested, though
the design does not depend on it — the fallbacks in §1 handle a terminal that answers
nothing.

---

## Environment findings (incidental but relevant)

- **Git upgraded 2.11.0 (Dec 2016) → 2.55.0.3** on Windows via winget. The old
  version predates usable `git worktree move/remove` and
  `worktree list --porcelain` (2.17+), which firstmate depends on heavily.
- **WSL is Ubuntu 20.04.6**, EOL since April 2025. gcc 9.4, git 2.25.1, clang 10 —
  all old. Fine for spike work; worth upgrading before it becomes the dev
  environment.
- scoop present but stale (v0.3.1, buckets ~2023). winget works.

### Windows toolchain — the `windows-gnu` path needs external MinGW

Installed rustup 1.29.0 / rustc 1.98.0. Default toolchain is
`stable-x86_64-pc-windows-msvc`, which cannot link — no MSVC compiler is present
(the Windows SDK is, but that ships no compiler).

Switched to `stable-x86_64-pc-windows-gnu` to avoid a 2–3 GB VS Build Tools
dependency. That alone is **not sufficient** — the build fails at `parking_lot_core`:

```
error: error calling dlltool 'dlltool.exe': program not found
```

rustup's `windows-gnu` toolchain does not bundle MinGW binutils. It needs an external
MinGW-w64. Installed **WinLibs POSIX MSVCRT** (gcc 16.1.0) — the MSVCRT variant
deliberately, because Rust's `x86_64-pc-windows-gnu` links MSVCRT, not UCRT; the UCRT
variant would mismatch.

This is worth writing into the project's setup docs. Windows contributors will hit it
immediately, and the error message does not suggest the fix. The MinGW install also
supplies the `cc` that tree-sitter grammars need on Windows, so it is required either
way — it is not purely a linker workaround.
