# crowsnest

A terminal code viewer for reviewing what your agents did — git status, branch-vs-fork-point
diffs, per-line blame, syntax highlighting and LSP — running as a herdr pane next to
the agent terminals.

Built to sit alongside [firstmate](https://github.com/kunchenguid/firstmate), which
dispatches each crewmate into its own git worktree. crowsnest binds to that worktree
and follows the fleet.

**Status: Phase 5 done.** Tree and content panes, mouse-clickable, themed, git-aware —
status badges, branch and counts, baseline cycling with fork-point detection, a unified
hunk view, tree-sitter syntax highlighting for C, C++, Go, Python, and Rust, and
per-line blame. LSP and editing are next. See [DESIGN.md](DESIGN.md)
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
cargo run -- /path/to/repo
```

| | |
|---|---|
| `↑`/`↓` or `j`/`k` | move |
| `←`/`→` or `h`/`l` | collapse/expand, or scroll content sideways |
| `enter` / `space` | open file, or fold directory |
| click | same — fold a directory, open a file |
| scroll wheel | scrolls whatever is under the pointer |
| `tab` | switch pane |
| `g` / `G` | top / bottom |
| `r` | refresh tree and git state |
| `q` / `esc` | quit |

Git:

| | |
|---|---|
| `b` | cycle diff baseline — unstaged → staged → vs HEAD → **vs fork point** |
| `d` | toggle between the file and its diff |
| `c` | show only changed files |
| `]` / `[` | jump to next / previous changed file, wrapping |
| `a` | annotate: toggle the per-line blame gutter |
| `W` | write a commit-graph (offered when one is missing) |

Tree badges are git's own letters — `M`, `A`, `D`, `R`, `?` — and a `·` on a
collapsed directory means something inside it changed.

**Fork point** is the one worth knowing: it answers "I branched from somewhere — what
has this branch actually changed?" The base is resolved automatically, preferring the
branch's upstream, then `main`/`master`/`develop`/`trunk` (local or on `origin`), then
the nearest tag.

Not a git repository? crowsnest opens anyway as a plain file browser.

Everything is reachable from the keyboard alone — spike 0.2 has not yet confirmed
herdr forwards mouse events into plugin panes, so the mouse is an enhancement rather
than a dependency.

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

crowsnest asks the terminal via XTGETTCAP rather than reading `COLORTERM`, because
environment variables do not survive SSH. The practical benefit: a wrong or missing
`TERM` cannot downgrade your colours, since the terminal is answering for itself.

Run `--caps` locally and again over SSH inside herdr — if the answers differ, that is
where colours will look wrong.

Verified against a real WezTerm by [spikes/05-wezterm-harness](spikes/05-wezterm-harness/),
an automated harness driving `wezterm-mux-server` headlessly under both bash and zsh.

---

## Setup

**crowsnest is Linux only.** On Windows, run it under WSL — which is Linux, so
nothing is lost. A native Windows build would mean a parallel console-API
implementation of the raw-stdin terminal queries in `crowsnest-tui/src/query.rs`,
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

crowsnest needs a modern git for worktree support — as does firstmate, which depends
on `git worktree move` / `remove` and reliable `list --porcelain` (all 2.17+).

```bash
sudo apt install git
```

---

## A performance note that will bite you

**Never run crowsnest across a filesystem boundary.** Measured on the same repo, same
machine — WSL's 9p bridge (`/mnt/e/...`) versus native ext4:

| Operation | over 9p | native | |
|---|---|---|---|
| open repository | 36.80 ms | 0.37 ms | 99× |
| diff merge-base..HEAD | 162.39 ms | 0.63 ms | **258×** |
| status | 689.08 ms | 122.61 ms | 5.6× |

Run crowsnest where the code lives. Over SSH, that means on the remote. This is the
main reason the architecture is SSH-first.

Related: crowsnest will offer to write a git commit-graph on first open. On an
80k-commit repo this costs under a second once and makes merge-base **6.7× faster**.
Say yes.

---

## Repository layout

```
crowsnest/
├── DESIGN.md                     # the plan, phases, risks
├── Dockerfile                    # cross-compilation toolchain image
├── scripts/build.sh              # drives the target matrix
├── .github/workflows/release.yml # checks, then the same Dockerfile
├── crates/
│   ├── crowsnest-core/           # model: tree, document, hit map, config
│   ├── crowsnest-vcs/            # Vcs trait + gix backend (the only gix user)
│   └── crowsnest-tui/            # ratatui panes, event loop, terminal queries
├── spikes/
│   ├── FINDINGS.md               # what the spikes settled — read this
│   ├── 01-treesitter-c-nested/   # PASS — tree-sitter vs GCC nested functions
│   ├── 02-04-terminal-probe/     # pending — mouse + rendering under herdr
│   └── 03-gix-perf/              # PASS — gix API coverage and perf
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
