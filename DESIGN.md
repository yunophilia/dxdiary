# dxdiary

A terminal code viewer/editor in Rust, built to sit in a herdr pane and watch what
the firstmate crew is doing.

**dx** for the differentials -- what changed, against which baseline, per line. **diary**
for the record it keeps of an agent fleet's work. A future diary in the sense that
matters here: you read it to find out what happened before you have to live with it.

---

## 1. What this is

A VS Code-shaped TUI focused on **reading and reviewing code under version control**,
running as a herdr split pane alongside agent terminals.

Target capabilities:

| # | Capability | Notes |
|---|---|---|
| 1 | Working-tree status: untracked / staged / unstaged | VS Code SCM panel equivalent |
| 2 | Diff a branch against its fork point | `merge-base` semantics, not just HEAD |
| 3 | Per-line blame | GitLens inline-blame equivalent |
| 4 | Mouse-clickable UI | tree nodes, hunks, tabs, gutter |
| 5 | Syntax highlighting | tree-sitter |
| 6 | LSP: C, C++, Rust, Python, Go | bundled server configs |
| 7 | herdr plugin, worktree-aware | follows the firstmate fleet |

### Scoping assumption — stated explicitly

Every capability listed above is a **read** operation. Text editing (rope mutation,
undo tree, multi-cursor, save, LSP `didChange` sync) is the single most expensive
part of building an editor and the least useful for reviewing agent output — the
crew writes the code, you read it.

So: **Phases 0–6 build a read-only reviewer. Editing is Phase 7.** The buffer layer
is architected from day one so Phase 7 is an extension, not a rewrite (ropey from
the start, even though nothing mutates it yet).

If you want editing sooner, say so and Phase 7 moves up — but it roughly doubles the
timeline and the review loop works without it.

### Portability requirement: SSH-first, genuinely cross-platform

Two real environments, and it must run over SSH into either:

| | Home | Office |
|---|---|---|
| Host OS | Windows 11 | Ubuntu Linux |
| dxdiary runs on | WSL Ubuntu | Ubuntu |
| Terminal | WezTerm | WezTerm |
| herdr | WSL | native |

**dxdiary targets Linux only.** On the Windows box it runs under WSL, which is
Linux, so the second environment is not really a second platform. A native Windows
build would mean a parallel console-API implementation of the raw-stdin terminal
queries in §1 for a case already covered — deliberately deferred, not forgotten.

This is a first-class constraint, not a nice-to-have, and it drives several
decisions:

- **No local-only assumptions.** No GUI, no OS file dialogs, no clipboard that
  depends on a local display server. Clipboard goes through OSC 52 so it works
  across SSH.
- **Ask the terminal, do not sniff the environment.** Environment variables do not
  survive SSH unless `SendEnv`/`AcceptEnv` are configured, so they are the fallback,
  not the primary path.

  Detection is **XTGETTCAP** — `DCS +q 524742 ST`, querying the `RGB` terminfo
  capability, with `Tc` as the older alternative. DA1 (`CSI c`) is sent last as a
  sync marker: every terminal answers it and it cannot overtake the earlier replies,
  so its arrival ends the read deterministically rather than on a timeout.

  Measured against a real WezTerm (spike 05):

  | query | reply |
  |---|---|
  | `DCS +q 524742 ST` — XTGETTCAP `RGB` | `1+r524742=382F382F38` → `8/8/8` |
  | `CSI > q` — XTVERSION | `WezTerm 20240203-110809-5046fc22` |
  | `CSI ? u` — kitty keyboard | **no reply** |

  Priority is **queried → inferred → environment**. XTGETTCAP first; kitty keyboard
  support as a weaker fallback inference (every terminal implementing it is 24-bit,
  but WezTerm does not answer that query at all); environment variables last.

  Unix only — raw stdin reads with a timeout need `poll(2)`. On Windows the
  environment genuinely is reliable: Windows Terminal sets `WT_SESSION` and is
  truecolor, and nothing else there is worth probing.

  `dxdiary --caps` reports the answer, its source, and the terminal's identity.

  **Measured through a real SSH hop** (spike 0.2): `COLORTERM` and `TERM_PROGRAM` are
  both unset on the far side, because `ssh` forwards only `TERM` without
  `SendEnv`/`AcceptEnv`. Environment sniffing would have concluded `Indexed256` and
  downgraded every colour; XTGETTCAP queried through the tunnel and got truecolor,
  plus the terminal's name. This is the case the whole approach exists for.

- **Kitty keyboard protocol when available**, with `DISAMBIGUATE_ESCAPE_CODES` only.
  Key-release and repeat reporting would double event volume for no benefit, and
  every extra byte is a round trip. Flags are popped on exit *and* from the panic
  hook, guarded by an atomic so a double-restore cannot unbalance the terminal's
  own stack.

> **Correction.** An earlier draft of this section claimed WezTerm does not set
> `COLORTERM`, citing [wezterm#875](https://github.com/wezterm/wezterm/issues/875),
> and built colour detection on the kitty keyboard query as a result. Measurement
> reversed both halves: WezTerm **does** set `COLORTERM=truecolor`,
> `TERM_PROGRAM=WezTerm`, and `WEZTERM_EXECUTABLE` (that issue was fixed), and it
> does **not** answer the kitty keyboard query. The design above is what the
> measurements support.
- **Render efficiency matters more than locally.** Every redraw crosses the wire.
  ratatui already diffs frames, but avoid full-screen invalidation on scroll, and
  never repaint on a timer.
- **Mouse must use SGR (1006) encoding.** The legacy X10 encoding breaks past column
  223, which any real code pane exceeds. This also compounds Spike 0.2 — mouse events
  may have to survive SSH *and* herdr's multiplexing.
- **LSP servers run server-side**, co-located with the code. Natural fit, no design
  work needed, but `dxdiary doctor` must report the *remote* toolchain.
- **Ship a static musl build.** One file, `scp`-able to any Linux box, no glibc
  version negotiation with whatever you land on. That is the primary artifact.

---

## 2. The hard constraint: GCC nested functions

This is the highest-risk item in the whole request and it needs to be settled first.

**clangd cannot parse GCC nested functions.** Clang has never implemented the
extension ([llvm#9578](https://github.com/llvm/llvm-project/issues/9578), open since
2011; [llvm#130652](https://github.com/llvm/llvm-project/issues/130652)), and clangd
declined to special-case it ([clangd#607](https://github.com/clangd/clangd/issues/607)).
You get `function definition is not allowed here`.

Measured on the real compilers (Spike 0.1, see `spikes/FINDINGS.md`):

| Compiler | GCC-nested-function fixture |
|---|---|
| gcc 9.4 `-std=gnu11` | compiles and runs correctly |
| clang 10 `-fsyntax-only` | **exit 1** — `function definition is not allowed here` |

The damage is worse than one bogus diagnostic — the enclosing function degrades
entirely, and calls to the nested functions fall back to implicit declarations. But
it is **contained to the enclosing function**, not the rest of the translation unit:
symbols declared after the nested-function block still compile clean. `ccls` is also
clang-based and fails identically. No mature GCC-based C language server exists.

So "built-in LSP for C including the GCC nested-function extension" **cannot be
delivered via clangd**. That's an external constraint, not a design choice. The plan
works around it:

**Two-source model for C.** tree-sitter becomes the *authority* for anything it can
answer, clangd is *advisory*:

| Feature | Source for C |
|---|---|
| Syntax highlighting | tree-sitter (always) |
| Document outline / symbols | tree-sitter (always) |
| Go-to-def, same file | tree-sitter |
| Go-to-def, cross-TU | clangd |
| Hover types, completion | clangd |
| Diagnostics | clangd, **filtered** |

Plus a diagnostic filter that drops `function definition is not allowed here` and
suppresses clangd diagnostics **within the byte range of the enclosing function** of
any tree-sitter-detected nested function, since those are parse-recovery garbage.
Scoping the filter to the enclosing function rather than to end-of-file means C keeps
most of its real diagnostics.

> **Spike 0.1: PASS.** tree-sitter-c parses nested functions with zero ERROR nodes,
> reports them as real `function_definition` nodes at correct nesting depth, and
> finds every symbol after the nested block. The two-source model is viable.
> Details and one implementation gotcha in `spikes/FINDINGS.md`.

---

## 3. Stack

| Layer | Choice | Why |
|---|---|---|
| TUI | `ratatui` + `crossterm` | de-facto standard; crossterm captures mouse |
| Async | `tokio` | LSP needs concurrent stdio pumps |
| Git | **`gix` (gitoxide)** | pure Rust, has `blame` + merge-base |
| Text | `ropey` | ropes now so Phase 7 isn't a rewrite |
| Syntax | `tree-sitter` + grammars | incremental; the C fallback authority |
| LSP | `async-lsp` + `lsp-types` | tower-based, client side supported |

### Why gix over git2

`git2` binds libgit2 and drags in a C toolchain and vendored OpenSSL, which fights
the static musl build; gitoxide is pure Rust and links clean. It also already has
blame and merge-base, the two non-trivial operations here.

Cost: gix churns more than git2 across releases. Mitigation — **all git access goes
behind a `Vcs` trait in `dxdiary-vcs`**, nothing else in the workspace imports
`gix`. Swapping backends stays a one-crate change. Note the `gix` *CLI* is explicitly
marked unstable; the `gix` *crate* is the supported surface.

### Mouse: no free lunch

ratatui has **no built-in hit testing** — crossterm delivers `MouseEvent` with
column/row, and mapping that to a widget is on you. `ratatui-interact` exists but
gives widget-level regions; a code pane needs char-precision (click → line N, col M →
byte offset, accounting for scroll, tabs, wide CJK, and the gutter).

So: **own `HitMap`.** Render populates a per-frame `Vec<(Rect, HitTarget)>`; the
event loop resolves clicks against it. `HitTarget` is an enum — `TreeNode(path)`,
`Hunk(id)`, `GutterLine(n)`, `TextCell(line, col)`, `Tab(id)`, `BlameLine(n)`.
One mechanism covers all seven clickable surfaces.

---

## 4. Workspace layout

```
dxdiary/
├── crates/
│   ├── dxdiary-core/     # buffer, ropes, view state, HitMap, config
│   ├── dxdiary-vcs/      # Vcs trait + gix impl; status, diff, blame, merge-base
│   ├── dxdiary-syntax/   # tree-sitter: highlight, outline, C nested-fn detection
│   ├── dxdiary-lsp/      # client, server registry, per-language launch config
│   ├── dxdiary-tui/      # ratatui widgets, panes, layout, mouse routing
│   └── dxdiary-herdr/    # context JSON, event hooks, state dir
├── dxdiary-plugin.toml   # herdr manifest
└── src/main.rs
```

---

## 5. Git model

### DiffBaseline

The core abstraction. Everything in the diff pane is "current vs *some* baseline":

```rust
enum DiffBaseline {
    WorkingTree,        // unstaged: index → worktree
    Index,              // staged:   HEAD  → index
    Head,               // all local changes
    MergeBase(Rev),     // ← branch vs its fork point
    Ref(Rev),           // arbitrary branch/tag/commit
}
```

Cycle with `b`, matching herdr-file-viewer's muscle memory.

### Fork-point detection

Requirement 2 — "I branched from some commit on a branch or tag, what changed in
this branch" — is three-dot diff: `merge_base(base, HEAD)..HEAD`.

Resolving `base` automatically, in order:
1. `@{upstream}` if the branch tracks one
2. First match among configured trunk names — `main`, `master`, `develop`, `trunk`
3. Nearest reachable tag by commit distance
4. Prompt, with a fuzzy ref picker

Cache the resolved merge-base per `(branch, head_oid)`; invalidate on HEAD change.
Recomputing merge-base on every keystroke is the obvious perf trap.

### Commit-graph is mandatory, not an optimisation

Measured on git/git (81,966 commits), merge-base against a fork point 5,000 commits
back took **350 ms — over budget**. Writing a commit-graph took 0.93 s once and
dropped that to **52.6 ms, a 6.7× win**; blame improved 1.6× as a side effect.

So: on repo open, check for `.git/objects/info/commit-graph`. If absent, offer to
write it in the background. It is the single highest-leverage thing dxdiary can do
for git performance, and it costs under a second.

### Status

`gix` status → three buckets matching VS Code's SCM panel: **Untracked**, **Staged**,
**Changes**. File tree shows `M`/`A`/`D`/`?` badges. `c` filters to changed-only,
`]`/`[` jump between changed files.

Read-only for now — no staging from the UI in Phase 1–6. Staging is a mutation and
belongs with the Phase 7 discussion.

### Blame

`gix::blame::file()` per file, computed lazily on toggle (`g`), cached per
`(path, oid)`. Rendered as a gutter column: short hash, author, relative date. Click
a blame line → open that commit's diff.

Blame is **the one operation that cannot be made to hit budget by caching**. Measured
on git/git: 543 ms for a 1,931-line file, and 2,467 ms for a 7,858-line file — the
latter unchanged by the commit-graph. So the 1 s budget means *first visible hunks*,
not complete result: compute off the render thread and stream hunks in as they
resolve, top of file first. Never block a frame on blame.

Note the API is lower-level than git2's `blame_file()` — it wants an odb handle,
suspect commit, diff resource cache, and options assembled by hand. Wrap it once in
`dxdiary-vcs`.

---

## 6. LSP

Registry maps language → server, with `--version` probing at startup and a graceful
"server not found, tree-sitter only" degradation.

| Language | Server | Notes |
|---|---|---|
| Rust | `rust-analyzer` | cleanest integration, build against this first |
| Python | `pyright` or `ruff` + `pylsp` | pyright for types |
| Go | `gopls` | |
| C++ | `clangd` | needs `compile_commands.json` |
| C | `clangd` + tree-sitter | **see §2** — degraded, by necessity |

"Built in" = bundled *configuration* and auto-detection, not vendored binaries.
Shipping clangd/gopls/rust-analyzer inside the binary would be hundreds of MB and a
licensing mess. Detect on `PATH`, document the install, offer a `dxdiary doctor`
that reports what's missing.

Read-only phase needs only: `initialize`, `textDocument/didOpen`, `hover`,
`definition`, `references`, `documentSymbol`, `publishDiagnostics`. No `didChange`
until Phase 7 — which is a large simplification.

---

## 7. herdr integration

The manifest contract, from herdr's plugin docs:

```toml
id = "dxdiary"
name = "dxdiary"
version = "0.1.0"
min_herdr_version = "..."

[[panes]]
id = "viewer"
title = "dxdiary"
placement = "split"      # side-by-side with the agent terminal
command = ["dxdiary", "--herdr"]
width = "45%"

[[events]]
on = "worktree.created"
command = ["dxdiary-attach"]
```

Two things make this fit firstmate specifically:

**`HERDR_PLUGIN_CONTEXT_JSON` carries `worktree` and `agent`.** firstmate dispatches
each crewmate into an isolated worktree; the context JSON tells the pane which
worktree it's looking at. The viewer binds to that worktree automatically instead of
guessing from `cwd`.

**The `worktree.created` event hook.** When firstmate spawns a new crewmate,
dxdiary can auto-attach or refresh. The viewer follows the fleet without manual
navigation — which is the actual daily win here.

Also use: `HERDR_PLUGIN_STATE_DIR` to persist per-worktree cursor/scroll/baseline so
switching between crewmates restores where you were. `W` switches worktree manually.

Set `platforms = ["linux", "macos"]` — there is no Windows binary to declare.

---

## 8. Phases

### Phase 0 — Spikes (~1 week, do not skip)

These four can each invalidate a chunk of the design. Throwaway code, answers only.

- ~~**0.1 — tree-sitter-c vs GCC nested functions.**~~ **PASS.** Clean parse, nested
  functions as real nodes at correct depth, post-block symbols intact. §2 viable.
- ~~**0.2 — Does herdr forward mouse events into plugin panes?**~~ **PASS.** herdr
  forwards them and translates coordinates to pane-local, exactly — a click at screen
  column 250 arrives as column 223 with the pane starting at column 27. SGR survives
  past column 223 (the X10 ceiling). Requirement 4 works inside herdr.
- ~~**0.3 — gix perf.**~~ **PASS.** Full API coverage, compiled first try. Diff is
  flat and fast at any history depth. merge-base needs a commit-graph; blame needs
  streaming. Cross-filesystem access (9p) is catastrophic — 258× slower diff.
- ~~**0.4 — Rendering under herdr.**~~ **PASS.** Truecolor detected, box-drawing and
  unicode render correctly, resize delivered — locally *and* through a real SSH hop.

**Phase 0 is complete.** All four spikes pass; see `spikes/FINDINGS.md`.

### ~~Phase 1 — Skeleton~~ — **done**
Workspace (`dxdiary-core`, `dxdiary-tui`, binary), ratatui event loop, file tree
+ content pane, `HitMap` and mouse routing, config with theming. Renders a repo,
click-navigable, no git yet. 38 tests; clippy and fmt clean.

Two things worth carrying forward:

- **The view layer is tested headlessly** against ratatui's `TestBackend`. The whole
  click path — render, register regions, resolve a coordinate, act — runs with no
  TTY, so it works in CI. That was the part most likely to rot silently.
- **Redraws happen on events, never on a timer**, and input bursts are drained before
  repainting. Idling is completely silent, which matters when every frame crosses an
  SSH connection.

### ~~Phase 2 — Git status + diff~~ — **done**
`dxdiary-vcs` with the `Vcs` trait and a gix backend; status buckets; `DiffBaseline`
with fork-point detection; commit-graph check and `W` to write one; badges in the tree
with directory roll-up; branch and counts in the status bar; `b` / `c` / `]` / `[`.
80 tests, including integration tests that build real repositories with the git CLI —
merge-base is cross-checked against `git merge-base` rather than only against itself.

Plus the hunk renderer: `dxdiary-vcs::diff` assembles unified hunks from gix's
slider-heuristic line diff, and the content pane renders them with dual line-number
gutters. Opening a changed file lands on its diff; `d` toggles back to the file.
Changing baseline recomputes the open diff. 111 tests.

Three things worth knowing:

- **Path spaces differ.** git reports repository-relative paths; the file tree holds
  absolute ones. Mixing them silently produced an empty changed-only filter — badges
  rendered correctly because that path converted, while the filter did not. Any new
  comparison against git output needs the same conversion.
- **Selection indexes the *visible* rows**, not `tree.rows()`. The changed-only filter
  would otherwise leave the cursor on a hidden row. `App::real_row()` maps back.
- **Viewport sizes are applied before drawing, not after.** They were being written
  at the end of `render`, so the first frame clamped scroll against a placeholder
  height of 1 and hid each pane's first row. Scroll also needed an upper bound —
  keeping the cursor visible is not enough on its own, because a pane that grows
  leaves a stale offset in place with blank rows stranded at the bottom.

### ~~Phase 3 — Syntax~~ — **done**
`dxdiary-syntax`: tree-sitter for C, C++, Go, Python, and Rust; per-line character
spans mapped to eleven theme roles; and the nested-function detector that §2's
diagnostic filter needs. Both the file view and the diff view are highlighted.

Four things worth knowing:

- **The C++ query only covers C++-specific nodes.** The grammar is a superset of C
  and its query assumes C's is prepended — without that, `int main() { return 0; }`
  highlights nothing at all. `Language::highlight_query` concatenates them.
- **Diff sides are highlighted as whole files, not line by line.** `FileDiff` carries
  `old_text` and `new_text` for exactly this: highlighting a diff line in isolation
  gets multi-line strings and block comments wrong, which is precisely the code a
  reviewer is squinting at. The renderer looks spans up by each side's line number.
- **Added and removed lines keep their diff colour**; only context lines take syntax
  colour. The change is what the eye needs first.
- Spans are in **characters, not bytes**, because horizontal scrolling slices by
  character — byte offsets desynchronise on any non-ASCII line.

Grammars disagree about capture names (Rust has no `number`; integers are `constant`),
so roles are matched on the capture prefix rather than the full name.

### ~~Phase 4 — LSP~~ — **done**
`dxdiary-lsp`: JSON-RPC over stdio, a server registry with `dxdiary --doctor`,
and the C diagnostic filter from §2. Diagnostics show as a gutter marker; `K` asks
for hover. Verified end to end against a real rust-analyzer, and against a mock
server for everything a real one cannot be relied on to do.

**No async runtime.** A reader thread feeds a channel and the UI polls — the same
shape blame uses. Adding tokio for one subprocess would be a large dependency for no
benefit in a synchronous event loop.

**Full-text sync, never incremental.** Phase 4 was read-only and skipped `didChange`
entirely; Phase 7 added it as the whole document, sent after 150 ms of quiet rather
than per keystroke. The range-less form is valid whatever sync kind the server
announced, and on a pipe to a local process the bandwidth is nothing. An incremental
edit log would have to mirror the rope exactly, and a mismatch there is silent until
diagnostics land on the wrong line.

**Presence on `PATH` is not availability.** `~/.cargo/bin/rust-analyzer` is a rustup
*shim*: it exists, it is executable, and when the component is not installed it
prints `Unknown binary` **to stderr and exits 0**. Neither the file test nor the exit
code catches that. `registry::probe` runs `--version` and requires non-empty *stdout*,
and `doctor` reports three states — ok, missing, and present-but-broken — with the
command to fix each.

### ~~Phase 5 — Blame~~ — **done**
`dxdiary-vcs::blame` over `gix::blame`, rendered as a gutter toggled with `a`.

Computed **on a background thread**, as §5 required: spike 0.3 measured 543 ms for a
1,931-line file and 2.5 s for a 7,858-line one, and a commit-graph does not help. The
keypress returns immediately and the event loop collects the result; a test asserts
the blame is still absent right after the key, so a future refactor cannot quietly
make it synchronous again.

The worker opens its own repository handle — `gix::Repository` is `Send` but not
`Sync`, which is why the `Vcs` trait requires only `Send`.

Runs of lines from one commit show attribution on the first line only, as GitLens and
`tig blame` do; repeating it down a whole function buries where authorship actually
changes. Output was checked line-for-line against `git blame` on a two-author fixture.

Click-to-commit is not implemented — it needs a commit-detail view that does not exist
yet.

### ~~Phase 6 — herdr plugin~~ — **done**
`dxdiary-plugin.toml` declares a split pane, a zoomed pane, two actions, and a
`worktree.created` hook. `dxdiary-herdr` reads `HERDR_PLUGIN_CONTEXT_JSON`.

**The worktree overrides the working directory**, and that is the whole feature. A
plugin pane inherits *herdr's* cwd, not the crewmate's, so trusting cwd would show the
wrong tree. Verified by launching from `/tmp` with only the context JSON pointing at a
worktree: dxdiary opened the worktree.

The context parser tries `worktree`, `worktree_path`, and `cwd`, and accepts either a
string or an object with a `path` — herdr has spelled it differently across versions,
and a pane that silently fails to bind is worse than one that tries the likely keys.
Malformed JSON degrades to "not bound" rather than failing to start.

Per-worktree state is keyed on a hash of the **full** worktree path, not its basename:
two crewmates working on `feature` under different parents would otherwise collide and
send you to the wrong file.

### ~~Phase 7 — Editing~~ — **done**
Rope-backed buffer (`dxdiary-core::buffer`), grouped undo/redo, modal editing, and
an atomic save. `i` enters insert, `Esc` leaves, `u`/`Ctrl+R` undo and redo, `x` and
`D` delete a character and a line, `Ctrl+S` writes.

**Modal, not always-insert.** dxdiary is a reviewer first: `d`, `b`, `c`, and `a`
are single-key commands, and making them all modifiers to free the alphabet would be
the wrong trade for the common case. `Ctrl+C` quits from either mode, so a session is
always escapable.

**Undo is grouped by pause, not per keystroke.** Typing a word and pressing undo
removes the word. The clock is passed in rather than read inside the buffer, so the
coalescing logic is testable rather than timing-dependent.

**The buffer is the single source of truth** for text once a file is open. `Document`
keeps the non-text cases and the path; rendering reads the buffer, so an edit is
visible immediately and there is never a second copy to drift.

**Save writes a sibling temp file and renames.** An interrupted save cannot leave a
truncated source file behind, and rename within a directory is atomic everywhere
dxdiary runs.

A goal column is preserved across short lines — moving down through a short line and
back returns to the original column, which is the thing naive implementations get
wrong.

**Live diagnostics.** Edits reach the language server as full-text `didChange` after a
150 ms pause, `ctrl-s` flushes any pending change before `didSave`, and opening another
file sends `didClose`. Until the server answers, the previous markers stay where they
were — briefly a line off after an inserted newline, which is what every editor shows
and far less confusing than markers that vanish while typing.

**Unsaved edits are never discarded silently.** `q` or opening another file with a
dirty buffer refuses once and says why; the same action again is the confirmation.
Arming is per action, so a refused quit does not license the next open. `ctrl-c`
remains unconditional — a runaway session must always be escapable, and that is the
one key documented as such.

### Phase 8 — finding things — **done**

`/` searches the open file, live as you type, with `n`/`N` to step and wrapping
announced rather than silent. `:` goes to a line. Literal matching, not regex: the
question being asked is "where else does this symbol appear", which is what `/` in
`less` has always answered without a regex syntax to get wrong. Regex can come later
behind its own prefix if it earns one.

Smart case, as vim and ripgrep do it. Case folding is compared per character rather
than by lowercasing whole lines, because folding can change a string's length and
every offset here is an index into the original.

Matches shade the **background**. A foreground highlight would have to fight the
syntax colour that tells you what you are looking at.

Both the match offsets and the syntax spans are raw-character indices mapped through
one column table from `render_line_mapped`, which is what makes them agree on a
tab-indented line.

A prompt owns the status line while it is open, as in `less` and vim, and draws its
own caret -- the terminal's cursor is parked out of the way in raw mode.

`esc` dismisses before it quits: with a search on screen the reflex is to press it to
clear the highlight, and losing the session to that reflex would be its own small
disaster. `q` stays unconditional.

### Phase 9 — a cursor you can aim — **done**

`ctrl-]` goes to a definition and `ctrl-o` comes back, through a stack so a jump into
someone else's crate is not a one-way trip. A definition in another file reveals and
opens it; one that is not on disk — a location inside an archive, or a virtual
document — is reported rather than attempted.

The reply shape is not one thing. The protocol allows a bare `Location`, an array of
them, an array of `LocationLink`, or null, and the five servers dxdiary ships specs for
use at least three. `LocationLink` carries two ranges and the useful one is
`targetSelectionRange`, the symbol, not `targetRange`, its whole body. rust-analyzer
answers this way, so that path is exercised against a real server and not only the
mock.

**Making this work needed a cursor that could be aimed.** `h`/`l` panned the viewport,
so the buffer cursor's column only ever changed in insert mode — which meant `K` and
`ctrl-]` could only ask about column zero, and would have resolved whatever happened to
start the line. Now the arrows move the cursor with the viewport following, `shift`
plus an arrow pans for reading a long line, and the cursor is drawn as a reversed cell
so you can see what you are about to ask about. Moving between lines carries the buffer
cursor along, preserving its goal column.

A reversed cell rather than a themed colour: the caret has to be obvious on top of
whatever syntax and search have already done to that cell, and reversing is the one
thing that always contrasts.

The content pane's text width now has one definition, in `App`, because the horizontal
scroll is clamped against it on a keystroke — before any frame exists. A zero width
means nothing has been drawn yet; following the cursor against that scrolled the line
clean off screen, which the test suite caught.

## 9. Risks

| Risk | Severity | Status |
|---|---|---|
| clangd + GCC nested functions | ~~High~~ | **Resolved** — §2 two-source model validated by Spike 0.1 |
| herdr doesn't pass mouse to panes | ~~High~~ | **Resolved** — Spike 0.2: forwarded, coordinates pane-local, SGR past column 223 |
| Blame slow on large files | Medium | **Confirmed real** — 2.5 s worst case measured; mitigated by streaming, not caching |
| gix API churn | Medium | `Vcs` trait isolates it to one crate |
| Cross-filesystem access | Medium | **Measured** — 258× penalty on 9p; SSH-first architecture avoids it |
| Scope creep into a real editor | **High** | Phase 7 is explicitly deferred |

The last one is the one that actually kills projects like this. Phases 1–2 give you
something you'd use every day. Ship that before touching LSP.

---

## 10. Prior art worth reading before writing code

- **helix** — ratatui-adjacent, tree-sitter + LSP in Rust. Closest reference for §3–6.
- **gitui** — gix-based TUI, good reference for git pane ergonomics.
- **smarzban/herdr-file-viewer** — already does §5's baseline-flipping and worktree
  switching. Read it first; it may cover enough that this project narrows to
  "file-viewer + LSP + blame".
- **persiyanov/herdr-reviewr** — the comment-back-to-agent loop.
