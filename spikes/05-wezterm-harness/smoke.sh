#!/usr/bin/env bash
# Interactive smoke test: drive the real TUI in a real WezTerm and read the
# screen back. run.sh proves capability detection works against a real
# terminal; this proves the editor runs, decodes keys in raw mode, and gives
# the shell back on quit.
#
#   ./smoke.sh [path-to-dxdiary] [repo-to-open]
#
# Exits non-zero if any assertion fails.
set -uo pipefail

TOOLS="${TOOLS:-$HOME/tools}"
WEZTERM="$TOOLS/squashfs-root/usr/bin/wezterm"
MUX="$TOOLS/squashfs-root/usr/bin/wezterm-mux-server"
DX="${1:-$HOME/.local/bin/dxdiary}"
REPO="${2:-$HOME/dxdiary}"
# How long any one expected change may take to appear. Generous on purpose: a
# cold CI runner is far slower than a laptop, and a high ceiling only costs
# time when something is actually broken.
TIMEOUT="${SMOKE_TIMEOUT:-20}"

pass=0; fail=0
ok()  { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }

for f in "$WEZTERM" "$MUX" "$DX"; do
  [ -x "$f" ] || { echo "missing: $f" >&2; exit 1; }
done

# wezterm-mux-server needs a writable runtime directory for its socket. WSL
# does not always create /run/user/$UID -- it is gone after the distro
# restarts, and XDG_RUNTIME_DIR still points at it -- so fall back to one that
# always exists rather than failing with a bare "Permission denied".
if [ ! -w "${XDG_RUNTIME_DIR:-/nonexistent}" ]; then
  XDG_RUNTIME_DIR="$HOME/.cache/dxdiary-runtime"
  mkdir -p "$XDG_RUNTIME_DIR"
  chmod 700 "$XDG_RUNTIME_DIR"
  export XDG_RUNTIME_DIR
fi

# Its own mux config, rather than relying on run.sh having been run first:
# the two scripts are independent entry points and an order dependency between
# them is invisible until someone runs this one alone.
mkdir -p "$HOME/.config/wezterm"
cat > "$HOME/.config/wezterm/wezterm.lua" <<'LUA'
return {
  unix_domains = { { name = 'default' } },
  default_prog = { '/bin/bash', '--norc', '--noprofile' },
  check_for_updates = false,
}
LUA

pkill -f wezterm-mux-server 2>/dev/null
"$MUX" --daemonize >/dev/null 2>&1
# Wait for the socket to answer rather than sleeping at it.
for _ in $(seq 1 40); do
  pgrep -f wezterm-mux-server >/dev/null \
    && "$WEZTERM" cli list >/dev/null 2>&1 && break
  sleep 0.5
done
"$WEZTERM" cli list >/dev/null 2>&1 \
  || { echo "mux server never became ready" >&2; exit 1; }
trap 'pkill -f wezterm-mux-server 2>/dev/null; rm -f "$REPO/smoke-target.rs"' EXIT

PANE=$("$WEZTERM" cli spawn --new-window -- /bin/bash --norc --noprofile) || exit 1

screen() { "$WEZTERM" cli get-text --pane-id "$PANE" 2>/dev/null; }
dump()   { printf '%s\n' "$1" | sed -n '1,8p' | sed 's/^/       | /'; }

# Enter is CR: that is what a terminal sends in raw mode, and LF does not
# decode as KeyCode::Enter.
keys() { "$WEZTERM" cli send-text --pane-id "$PANE" --no-paste "$1"; sleep 0.2; }

# Poll the screen until a pattern appears, rather than sleeping a fixed amount
# and hoping. Fixed sleeps are what made the first draft of this script flaky
# on a slower machine -- the same lesson spike 0.5 already recorded once, and
# which I had to learn twice.
#
#   $1  grep flag: -F literal, -E regex
#   $2  pattern
#   $3  "gone" to wait for the pattern to disappear instead of appear
wait_for() {
  local flag="$1" pat="$2" want_gone="${3:-}" out="" deadline=$((SECONDS + TIMEOUT))
  while [ "$SECONDS" -lt "$deadline" ]; do
    out=$(screen)
    if [ "$want_gone" = gone ]; then
      grep -q "$flag" -- "$pat" <<<"$out" || { printf '%s' "$out"; return 0; }
    else
      grep -q "$flag" -- "$pat" <<<"$out" && { printf '%s' "$out"; return 0; }
    fi
    sleep 0.3
  done
  printf '%s' "$out"
  return 1
}

check() {
  local o
  if o=$(wait_for -F "$2"); then ok "$1"
  else bad "$1"; printf '       wanted: %s\n' "$2"; dump "$o"; fi
}
check_re() {
  local o
  if o=$(wait_for -E "$2"); then ok "$1"
  else bad "$1"; printf '       wanted /%s/\n' "$2"; dump "$o"; fi
}
check_gone() {
  local o
  if o=$(wait_for -F "$2" gone); then ok "$1"
  else bad "$1"; printf '       expected gone: %s\n' "$2"; dump "$o"; fi
}

echo
echo "=== dxdiary interactive smoke test ========================="
printf '  binary: %s\n  repo:   %s\n  budget: %ss per step\n\n' "$DX" "$REPO" "$TIMEOUT"

# A known file, so assertions can name its contents rather than hoping: two
# `alpha` matches and one `beta`.
printf 'fn alpha() {}\nfn beta() {}\nfn alpha_two() {}\n' > "$REPO/smoke-target.rs"
"$WEZTERM" cli send-text --pane-id "$PANE" --no-paste \
  "cd $REPO && exec $DX smoke-target.rs"$'\n'

check    "the TUI painted its panes"                    "┌"
check    "it opened the file named on the command line" "smoke-target.rs"
check    "the file's contents are shown"                "fn alpha()"
# Not the branch name: CI checks out a detached HEAD, and anyone on a feature
# branch would see a false failure. The baseline label renders only when a
# repository actually attached, which is the claim.
check    "the git layer attached"                       "unstaged"

# --- search ---------------------------------------------------------------
keys $'\t'
keys "/"
keys "alpha"
check    "the search prompt echoes what is typed"       "/alpha"
keys $'\r'
check_re "search reports how many matched"              "2 matches for"

keys "n"
check_re "n steps and shows position/total"             "2/2"
keys "n"
check    "stepping past the last wraps, and says so"    "wrapped"

keys $'\e'
check    "esc clears the search without quitting"       "search cleared"

# --- go to line -----------------------------------------------------------
keys ":"
keys "3"
check    "the goto prompt echoes the number"            ":3"
keys $'\r'
check    "it reports the line it went to"               "line 3"

# --- git ------------------------------------------------------------------
keys "b"
check    "b cycles the diff baseline"                   "staged"

# --- quit -----------------------------------------------------------------
# A TUI that cannot restore the terminal is worse than one that will not
# start, so this waits for the panes to actually disappear.
keys "q"
check_gone "q quit and restored the terminal"           "┌"

"$WEZTERM" cli kill-pane --pane-id "$PANE" >/dev/null 2>&1
echo "============================================================"
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
