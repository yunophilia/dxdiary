# dxdiary

*dx*, the differentials — what changed, against which baseline, per line. *diary*, the
record of what an agent fleet did to your tree.

A terminal code viewer for reviewing what your agents did — git status, branch-vs-fork-point
diffs, per-line blame, syntax highlighting and LSP — running as a herdr pane next to
the agent terminals.

Built to sit alongside [firstmate](https://github.com/kunchenguid/firstmate), which
dispatches each crewmate into its own git worktree. dxdiary binds to that worktree
and follows the fleet.

**Status: all phases done.** Tree and content panes, mouse-clickable, themed, git-aware —
status badges, branch and counts, baseline cycling with fork-point detection, a unified
hunk view, tree-sitter syntax highlighting for C, C++, Go, Python, and Rust, and
per-line blame, and LSP for all five languages. Editing works too. See [DESIGN.md](DESIGN.md)
for the plan and [spikes/FINDINGS.md](spikes/FINDINGS.md) for what the spikes settled.

```
┌ hunk-demo ───────────────┐┌ lib.rs  diff · 1 hunk(s), +2 -2 ─────────────┐
│· ▾ src                   ││@@ -1,9 +1,9 @@                               │
│M     lib.rs              ││ 1  1   pub fn greet(name: &str) -> String {  │
│                          ││ 2    -     format!("Hello, {name}!")         │
│                          ││    2 +     format!("Hi there, {name}!")      │
│                          ││ 3  3   }                                     │
└──────────────────────────┘└──────────────────────────────────────────────┘
 main ~1 │ unstaged │ src/lib.rs · 1 hunk(s), +2 -2  1:11
```

```bash
dxdiary /path/to/repo      # a directory opens as the tree
dxdiary src/main.rs        # a file opens its repository, with that file showing
dxdiary                    # the current directory
```

| | |
|---|---|
| `↑`/`↓` or `j`/`k` | move |
| `←`/`→` or `h`/`l` | collapse/expand in the tree, or move the cursor along the line |
| `shift`+`←`/`→` | pan a long line without walking the cursor along it |
| `enter` / `space` | open file, or fold directory |
| click | same — fold a directory, open a file |
| scroll wheel | scrolls whatever is under the pointer |
| `tab` | switch pane |
| `g` / `G` | top / bottom |
| `r` | refresh tree and git state |
| `q` | quit |
| `esc` | clear the search, or quit when there is nothing to clear |

Finding things:

| | |
|---|---|
| `/` | search the open file — live as you type, `esc` cancels and puts the cursor back |
| `n` / `N` | next / previous match, wrapping |
| `*` | search every file in the worktree; results take the left pane |
| `:` | go to a line number |

Search is literal, not regex, and smart-cased: an all-lowercase query matches any
case, and typing a capital means you meant it. Matches are shaded, the one you are on
more strongly, and the count sits in the status bar.

`*` searches the whole tree with the same matching rules. Results list as
`name:line` plus the matching code, and the left pane widens to make room for it —
a line of code does not fit in a third of an 80-column terminal. Enter or a click
opens the file at that line and highlights the same query in it, so you see the other
hits on the way past. The selected row's full path is in the status bar, since the rows
show only a basename. `esc` gives the tree back.

Skipped: `.git`, `target`, `node_modules`, `.venv`, `__pycache__` — the same list the
tree hides, so a result can never point at a file you cannot otherwise reach. Binary
files and anything over `max_file_bytes` are skipped too, and the list caps at 500 hits
and says when it did.

Editing:

| | |
|---|---|
| `i` | insert mode · `esc` leaves |
| `u` / `ctrl-r` | undo / redo |
| `x` / `D` | delete character / line |
| `ctrl-s` | save |
| `ctrl-c` | quit from any mode |

Undo groups by pause, so typing a word and undoing removes the word. `●` in the
title means unsaved. Saving writes a temp file and renames, so an interrupted save
cannot truncate your source.

Quitting or opening another file with unsaved edits refuses once and tells you; do
it again to discard. `ctrl-c` skips the question.

Git:

| | |
|---|---|
| `b` | cycle diff baseline — unstaged → staged → vs HEAD → **vs fork point** |
| `d` | toggle between the file and its diff |
| `c` | show only changed files |
| `]` / `[` | jump to next / previous changed file, wrapping |
| `a` | annotate: toggle the per-line blame gutter |
| `W` | write a commit-graph (offered when one is missing) |

Language servers:

| | |
|---|---|
| `K` | hover information at the cursor |
| `ctrl-]` | go to definition — follows into another file |
| `ctrl-o` | come back from a jump |
| gutter | `✗` error, `!` warning, `i` info |

Diagnostics are live: the server sees each edit after a short pause and again on
save, so markers follow what you type rather than what is on disk.

`K` and `ctrl-]` ask about the column the cursor is on, which is the reversed cell on
the current line. A jump into another file reveals it in the tree, and `ctrl-o` walks
back through where you have been.

```bash
cargo run -- --doctor
```

Reports which servers are installed **and actually run** — a rustup shim is on `PATH`,
executable, and exits 0 while being unusable, so presence alone is not checked.
Missing servers are not fatal: tree-sitter still provides highlighting and outline.

For C, clangd cannot parse GCC nested functions at all, so its diagnostics are
discarded inside any function containing one and tree-sitter is authoritative. The
status bar says when anything was hidden and why.

Tree badges are git's own letters — `M`, `A`, `D`, `R`, `?` — and a `·` on a
collapsed directory means something inside it changed.

**Fork point** is the one worth knowing: it answers "I branched from somewhere — what
has this branch actually changed?" The base is resolved automatically, preferring the
branch's upstream, then `main`/`master`/`develop`/`trunk` (local or on `origin`), then
the nearest tag.

Not a git repository? dxdiary opens anyway as a plain file browser.

Everything is reachable from the keyboard alone. Spike 0.2 confirmed herdr forwards
mouse events into plugin panes, and over SSH, but an unknown terminal at the far end
of a hop may not — so the mouse is an enhancement rather than a dependency.

### Terminal capabilities

```bash
cargo run -- --caps
```

Reports the detected colour depth, **where the answer came from**, the terminal's
identity, and whether the kitty keyboard protocol is active.

```
colour depth       TrueColor
  source           XTGETTCAP — the terminal stated it
terminal           WezTerm 20240203-110809-5046fc22
kitty keyboard     no — legacy key encoding
```

dxdiary asks the terminal via XTGETTCAP rather than reading `COLORTERM`, because
environment variables do not survive SSH. The practical benefit: a wrong or missing
`TERM` cannot downgrade your colours, since the terminal is answering for itself.

Run `--caps` locally and again over SSH inside herdr — if the answers differ, that is
where colours will look wrong.

Verified against a real WezTerm by [spikes/05-wezterm-harness](spikes/05-wezterm-harness/),
an automated harness driving `wezterm-mux-server` headlessly under both bash and zsh.

Two scripts live there. `run.sh` checks capability detection; `smoke.sh` drives the
editor itself — opens a file, searches it, steps the matches, goes to a line, cycles
the baseline, and checks the terminal is restored on quit:

```bash
./spikes/05-wezterm-harness/run.sh ~/.local/bin/dxdiary
./spikes/05-wezterm-harness/smoke.sh ~/.local/bin/dxdiary ~/dxdiary
```

---

## Setup

**dxdiary is Linux only.** On Windows, run it under WSL — which is Linux, so
nothing is lost. A native Windows build would mean a parallel console-API
implementation of the raw-stdin terminal queries in `dxdiary-tui/src/query.rs`,
for a use case already covered.

### Native toolchain — for fast local iteration

Release artifacts come from Docker, but a native toolchain gives a much tighter
edit-compile loop than a container round-trip. Worth having on your dev box:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Plus a C compiler — tree-sitter grammars are C and build via the `cc` crate:

```bash
sudo apt install build-essential
```

### Docker — the release build system

Every target comes from one `Dockerfile`, so a CI artifact and a local build are the
same binary. Nothing but Docker is required on the host — no Rust, no C compiler.

```bash
./scripts/build.sh
```

Or a single target:

```bash
./scripts/build.sh musl
```

| Alias | Triple | Notes |
|---|---|---|
| `musl` | `x86_64-unknown-linux-musl` | **static** — scp it anywhere, no glibc matching |
| `linux-gnu` | `x86_64-unknown-linux-gnu` | |
| `arm64` | `aarch64-unknown-linux-gnu` | ARM servers |

Binaries land in `dist/<triple>/`.

`musl` is the one to reach for in an SSH-first workflow: one static file, no runtime
dependencies, no glibc version negotiation with whatever box you land on.

### Git

dxdiary needs a modern git for worktree support — as does firstmate, which depends
on `git worktree move` / `remove` and reliable `list --porcelain` (all 2.17+).

```bash
sudo apt install git
```

---

## A performance note that will bite you

**Never run dxdiary across a filesystem boundary.** Measured on the same repo, same
machine — WSL's 9p bridge (`/mnt/e/...`) versus native ext4:

| Operation | over 9p | native | |
|---|---|---|---|
| open repository | 36.80 ms | 0.37 ms | 99× |
| diff merge-base..HEAD | 162.39 ms | 0.63 ms | **258×** |
| status | 689.08 ms | 122.61 ms | 5.6× |

Run dxdiary where the code lives. Over SSH, that means on the remote. This is the
main reason the architecture is SSH-first.

Related: dxdiary will offer to write a git commit-graph on first open. On an
80k-commit repo this costs under a second once and makes merge-base **6.7× faster**.
Say yes.

---

## Repository layout

```
dxdiary/
├── DESIGN.md                     # the plan, phases, risks
├── Dockerfile                    # cross-compilation toolchain image
├── scripts/build.sh              # drives the target matrix
├── .github/workflows/release.yml # checks, then the same Dockerfile
├── dxdiary-plugin.toml         # herdr plugin manifest
├── crates/
│   ├── dxdiary-core/           # model: tree, document, buffer, hit map, config
│   ├── dxdiary-vcs/            # Vcs trait + gix backend (the only gix user)
│   ├── dxdiary-syntax/         # tree-sitter highlighting, C nested-function scan
│   ├── dxdiary-lsp/            # JSON-RPC client, server registry, C filter
│   ├── dxdiary-herdr/          # plugin context: worktree binding, state dir
│   └── dxdiary-tui/            # ratatui panes, event loop, terminal queries
├── spikes/
│   ├── FINDINGS.md               # what the spikes settled — read this
│   ├── 01-treesitter-c-nested/   # PASS — tree-sitter vs GCC nested functions
│   ├── 02-04-terminal-probe/     # PASS — mouse forwarded through herdr and SSH
│   ├── 03-gix-perf/              # PASS — gix API coverage and perf
│   └── 05-wezterm-harness/       # automated terminal tests under bash and zsh
└── src/main.rs                   # the binary
```

## Running the spikes

```bash
cargo run --manifest-path spikes/01-treesitter-c-nested/Cargo.toml
```

```bash
cargo run --release --manifest-path spikes/03-gix-perf/Cargo.toml -- <repo> [base-ref] [file]
```

Spike 0.2/0.4 is interactive and must be run by hand in three contexts — see
[its README](spikes/02-04-terminal-probe/README.md).
