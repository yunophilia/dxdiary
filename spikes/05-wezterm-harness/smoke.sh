#!/usr/bin/env bash
# Interactive smoke test: drive the real TUI in a real WezTerm and read the
# screen back. The capability harness (run.sh) proves detection works against a
# real terminal; this proves the editor runs, responds to keys, and that the
# search and navigation features are live end to end.
set -uo pipefail

TOOLS="${TOOLS:-$HOME/tools}"
WEZTERM="$TOOLS/squashfs-root/usr/bin/wezterm"
MUX="$TOOLS/squashfs-root/usr/bin/wezterm-mux-server"
DX="${1:-$HOME/.local/bin/dxdiary}"
REPO="${2:-$HOME/dxdiary}"

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

pass=0; fail=0
ok()  { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }

for f in "$WEZTERM" "$MUX" "$DX"; do
  [ -x "$f" ] || { echo "missing: $f" >&2; exit 1; }
done

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
sleep 2
pgrep -f wezterm-mux-server >/dev/null || { echo "mux failed" >&2; exit 1; }
trap 'pkill -f wezterm-mux-server 2>/dev/null' EXIT

# One long-lived pane: state built up by earlier keys has to still be there for
# the later assertions, which is the point of an interactive test.
PANE=$("$WEZTERM" cli spawn --new-window -- /bin/bash --norc --noprofile) || exit 1
sleep 1
# A single known file, so assertions can name its contents rather than hoping.
printf 'fn alpha() {}\nfn beta() {}\nfn alpha_two() {}\n' > "$REPO/smoke-target.rs"
"$WEZTERM" cli send-text --pane-id "$PANE" --no-paste \
  "cd $REPO && $DX smoke-target.rs"$'\n'
sleep 3

keys()   { "$WEZTERM" cli send-text --pane-id "$PANE" --no-paste "$1"; sleep "${2:-1}"; }
screen() { "$WEZTERM" cli get-text --pane-id "$PANE"; }

dump() { printf '%s\n' "$1" | sed -n '1,8p' | sed 's/^/       | /'; }
# Fixed-string assertion.
check() {
  if grep -qF -- "$3" <<<"$2"; then ok "$1"
  else bad "$1"; printf '       wanted: %s\n' "$3"; dump "$2"; fi
}
# Regex assertion, for things like a 2/7 match counter where the digits matter.
check_re() {
  if grep -qE -- "$3" <<<"$2"; then ok "$1"
  else bad "$1"; printf '       wanted /%s/\n' "$3"; dump "$2"; fi
}

echo
echo "=== dxdiary interactive smoke test ========================="

out=$(screen)
check    "the TUI painted its panes"        "$out" "┌"
check    "it opened the file named on the command line" "$out" "smoke-target.rs"
check    "the file's contents are shown"    "$out" "fn alpha()"
# Not the branch name: CI checks out a detached HEAD, and anyone on a
# feature branch would see a false failure. The baseline label only
# renders when a repository actually attached, which is the claim.
check    "the git layer attached"           "$out" "unstaged"

# Search. The file has two `alpha` matches and one `beta`.
keys $'\t'
keys "/"
keys "alpha"
out=$(screen)
check    "the search prompt echoes what is typed" "$out" "/alpha"
keys $'\r'
out=$(screen)
check_re "search reports how many matched"  "$out" "2 matches for"

keys "n"
out=$(screen)
check_re "n steps and shows position/total" "$out" "2/2"
keys "n"
out=$(screen)
check    "stepping past the last wraps, and says so" "$out" "wrapped"

keys $'\e'
out=$(screen)
check    "esc clears the search without quitting" "$out" "search cleared"

# Go to line.
keys ":"
keys "3"
out=$(screen)
check    "the goto prompt echoes the number" "$out" ":3"
keys $'\r'
out=$(screen)
check    "it reports the line it went to"    "$out" "line 3"

# Baseline cycling, which needs the git layer alive.
keys "b"
out=$(screen)
check    "b cycles the diff baseline"        "$out" "staged"

# Quit cleanly. A TUI that cannot restore the terminal is worse than one that
# will not start, so this checks the shell actually came back.
keys "q" 2
out=$(screen)
if grep -qF '┌' <<<"$out"; then
  bad "q quit and restored the terminal"; dump "$out"
else
  ok "q quit and restored the terminal"
fi

"$WEZTERM" cli kill-pane --pane-id "$PANE" >/dev/null 2>&1
rm -f "$REPO/smoke-target.rs"
echo "============================================================"
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
