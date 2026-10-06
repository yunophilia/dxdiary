#!/usr/bin/env bash
# Automated terminal-capability tests against a real WezTerm.
#
# `wezterm-mux-server` is a headless multiplexer: it runs the real terminal
# emulation with no GUI and no X display, and `wezterm cli` can spawn panes,
# type into them, and read back what is on screen. That makes genuine terminal
# behaviour testable from a script -- which matters here, because capability
# detection is exactly the thing that cannot be checked with a unit test.
#
#   ./run.sh [path-to-dxdiary]
#
# Exits non-zero if any assertion fails.

set -uo pipefail

TOOLS="${TOOLS:-$HOME/tools}"
WEZTERM="$TOOLS/squashfs-root/usr/bin/wezterm"
MUX="$TOOLS/squashfs-root/usr/bin/wezterm-mux-server"
ZSH="$TOOLS/zsh/bin/zsh"

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

CN="${1:-$HOME/.cache/dxdiary-target/debug/dxdiary}"

pass=0
fail=0

ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }
note() { printf '       %s\n' "$1"; }

for f in "$WEZTERM" "$MUX" "$CN"; do
  [ -x "$f" ] || { echo "missing: $f" >&2; exit 1; }
done

# ---------------------------------------------------------------- server ---

cleanup() { pkill -f wezterm-mux-server 2>/dev/null; }
trap cleanup EXIT

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
sleep 2
pgrep -f wezterm-mux-server >/dev/null || { echo "mux server failed to start" >&2; exit 1; }

# Run a command in a fresh pane under `shell` and echo what lands on screen.
# Output has to be scraped rather than redirected: redirecting stdout to a file
# would make it not-a-tty, which is precisely the code path under test.
run_in_pane() {
  local shell_cmd="$1" command="$2" pane out
  # --new-window is required: without a window to target, `spawn` tries to use
  # the currently focused pane, which does not exist under a headless mux.
  pane=$("$WEZTERM" cli spawn --new-window -- $shell_cmd 2>/dev/null) || return 1
  [[ "$pane" =~ ^[0-9]+$ ]] || { echo "spawn failed: $pane" >&2; return 1; }
  sleep 1
  "$WEZTERM" cli send-text --pane-id "$pane" --no-paste "$command"$'\n'
  sleep 2
  out=$("$WEZTERM" cli get-text --pane-id "$pane")
  "$WEZTERM" cli kill-pane --pane-id "$pane" >/dev/null 2>&1
  printf '%s' "$out"
}

check() {
  local label="$1" haystack="$2" needle="$3"
  if grep -qF -- "$needle" <<<"$haystack"; then
    ok "$label"
  else
    bad "$label"
    note "expected to find: $needle"
  fi
}

# ----------------------------------------------------------------- tests ---

echo
echo "=== WezTerm capability tests ==============================="
"$WEZTERM" --version

for shell in bash zsh; do
  case "$shell" in
    bash) cmd="/bin/bash --norc --noprofile" ;;
    zsh)  [ -x "$ZSH" ] || { echo; echo "-- zsh: not installed, skipping"; continue; }
          cmd="$ZSH -f" ;;
  esac

  echo
  echo "-- $shell"
  out=$(run_in_pane "$cmd" "$CN --caps")

  check "truecolor detected"            "$out" "TrueColor"
  check "answer came from XTGETTCAP"    "$out" "XTGETTCAP"
  check "terminal identified as WezTerm" "$out" "WezTerm"

  # The point of querying rather than sniffing: a wrong or hostile TERM must
  # not change the answer, because the terminal itself is still truecolor.
  echo "-- $shell, with TERM=dumb"
  out=$(run_in_pane "$cmd" "TERM=dumb COLORTERM= $CN --caps")
  check "still truecolor despite TERM=dumb" "$out" "TrueColor"
  check "still sourced from XTGETTCAP"      "$out" "XTGETTCAP"
done

# Negative control: no terminal at all must degrade quietly, and must not leak
# query bytes into stdout.
echo
echo "-- no tty (piped)"
out=$("$CN" --caps 2>/dev/null)
check "falls back to environment" "$out" "guessed from environment"
if grep -q $'\033' <<<"$out"; then
  bad "no escape sequences leak into piped output"
else
  ok "no escape sequences leak into piped output"
fi

echo
echo "============================================================"
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
