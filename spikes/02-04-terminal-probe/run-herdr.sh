#!/usr/bin/env bash
# Spikes 0.2 + 0.4, automated.
#
# Does herdr forward mouse events into a pane, with usable coordinates, past
# column 223 -- and does any of it survive SSH? Previously this needed a human
# with a mouse. It does not: an SGR mouse report is just bytes on stdin, and
# `wezterm cli send-text` can inject them.
#
#   ESC [ < button ; col ; row M    press     (col/row 1-based)
#   ESC [ < button ; col ; row m    release
#   button 64 = wheel up, 65 = wheel down
#
# Every layer is real: wezterm-mux-server does genuine terminal emulation,
# herdr is the actual binary, the probe is a normal crossterm client, and the
# SSH hop is a real sshd. Chains tested:
#
#   wezterm -> herdr -> probe
#   wezterm -> ssh -> herdr -> probe
#
# The SSH variant runs a throwaway sshd as the current user on port 2222, with
# its own host key and authorized_keys under ~/.cache/spike-sshd. It never
# touches ~/.ssh/authorized_keys and needs no root.
#
#   ./run-herdr.sh            both variants (SSH skipped if sshd is missing)
#   ./run-herdr.sh local      local only
#
# Exits non-zero if any assertion fails.

set -uo pipefail

TOOLS="${TOOLS:-$HOME/tools}"
WEZTERM="$TOOLS/squashfs-root/usr/bin/wezterm"
MUX="$TOOLS/squashfs-root/usr/bin/wezterm-mux-server"
HERDR="${HERDR:-$HOME/.local/bin/herdr}"
PROBE="${PROBE:-$HOME/.cache/probe-target/release/spike-terminal-probe}"
SSHD_DIR="$HOME/.cache/spike-sshd"
SSH_PORT=2222

# Wider than 223 so the SGR-vs-X10 boundary is actually exercised. Any real
# code pane on a wide monitor crosses it.
COLS=260
ROWS=30
FAR_COL=250

MODE="${1:-all}"

pass=0; fail=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }
note() { printf '       %s\n' "$1"; }
skip() { printf '  \033[33mSKIP\033[0m %s\n' "$1"; }

for f in "$WEZTERM" "$MUX" "$HERDR" "$PROBE"; do
  [ -x "$f" ] || { echo "missing: $f" >&2; exit 1; }
done

# Two different matches, for two different reasons:
#
#   herdr  -> -x, matching the process name exactly. `pkill -f herdr` would
#             match this script's own path (run-herdr.sh) and SIGTERM the run.
#   wezterm-mux-server -> -f, matching the full command line. `-x` matches
#             comm, which the kernel truncates to 15 characters, so the
#             18-character name never matches and a stale server survives to
#             hold the socket ("os error 11" on the next start).
cleanup() {
  pkill -x herdr 2>/dev/null
  pkill -f wezterm-mux-server 2>/dev/null
  pkill -f "$SSHD_DIR/sshd_config" 2>/dev/null
}
trap cleanup EXIT
cleanup; sleep 1

# ------------------------------------------------------------------ setup ---

mkdir -p "$HOME/.config/wezterm"
cat > "$HOME/.config/wezterm/wezterm.lua" <<LUA
return {
  unix_domains = { { name = 'default' } },
  default_prog = { '/bin/bash', '--norc', '--noprofile' },
  initial_cols = $COLS,
  initial_rows = $ROWS,
  check_for_updates = false,
}
LUA

"$MUX" --daemonize >/dev/null 2>&1
sleep 2
pgrep -f wezterm-mux-server >/dev/null || { echo "mux server failed to start" >&2; exit 1; }

# Bring up an unprivileged sshd. Returns non-zero if it cannot.
start_sshd() {
  [ -x /usr/sbin/sshd ] || return 1
  mkdir -p "$SSHD_DIR"; chmod 700 "$SSHD_DIR"
  [ -f "$SSHD_DIR/host_ed25519" ] || ssh-keygen -q -t ed25519 -f "$SSHD_DIR/host_ed25519" -N '' -C spike-host
  [ -f "$SSHD_DIR/id" ] || ssh-keygen -q -t ed25519 -f "$SSHD_DIR/id" -N '' -C spike-client
  cp "$SSHD_DIR/id.pub" "$SSHD_DIR/authorized_keys"
  chmod 600 "$SSHD_DIR/authorized_keys" "$SSHD_DIR/id" "$SSHD_DIR/host_ed25519"

  cat > "$SSHD_DIR/sshd_config" <<CONF
Port $SSH_PORT
ListenAddress 127.0.0.1
HostKey $SSHD_DIR/host_ed25519
PidFile $SSHD_DIR/sshd.pid
AuthorizedKeysFile $SSHD_DIR/authorized_keys
PasswordAuthentication no
UsePAM no
StrictModes no
LogLevel ERROR
CONF
  /usr/sbin/sshd -f "$SSHD_DIR/sshd_config" -E "$SSHD_DIR/sshd.log" 2>/dev/null || return 1
  sleep 1
  pgrep -f "$SSHD_DIR/sshd_config" >/dev/null
}

ssh_cmd() {
  echo "ssh -tt -o StrictHostKeyChecking=no -o UserKnownHostsFile=$SSHD_DIR/known_hosts -i $SSHD_DIR/id -p $SSH_PORT 127.0.0.1"
}

# ------------------------------------------------------------- primitives ---

send()  { "$WEZTERM" cli send-text --pane-id "$1" --no-paste "$2"; }
click() { send "$1" $'\x1b[<0;'"$2"';'"$3"'M'; send "$1" $'\x1b[<0;'"$2"';'"$3"'m'; }
text()  { "$WEZTERM" cli get-text --pane-id "$1"; }

# Poll until `pattern` appears on the pane, up to `limit` seconds.
#
# Fixed sleeps made this suite flaky: herdr's startup time varies, and sending
# the next command into a pane that is not ready yet loses it silently.
wait_for() {
  local pane="$1" pattern="$2" limit="${3:-25}" i=0
  while [ "$i" -lt "$limit" ]; do
    grep -q "$pattern" <<<"$(text "$pane")" && return 0
    sleep 1
    i=$((i + 1))
  done
  return 1
}

# Column from the most recent L-down line. The probe reports 0-based columns.
last_down_col() { grep -o 'L-down *col=[0-9]*' <<<"$1" | grep -o '[0-9]*$' | tail -1; }

# Assert a click landed past the X10 ceiling.
#
# X10 mouse encoding adds 32 to each coordinate and sends it as one byte, so it
# cannot express a 1-based column above 223. Anything higher proves SGR (1006)
# is in use end to end. Read from the event log rather than the summary panel,
# so the check does not depend on where the panel happens to render.
assert_past_x10() {
  local label="$1" out="$2" zero_based one_based
  zero_based=$(last_down_col "$out")
  if [ -z "$zero_based" ]; then
    bad "$label"; note "no click event recorded at all"; return
  fi
  one_based=$((zero_based + 1))
  if [ "$one_based" -gt 223 ]; then
    ok "$label (column $one_based)"
  else
    bad "$label"; note "got 1-based column $one_based; X10 would cap at 223"
  fi
}

# --------------------------------------------------------------- the case ---

# run_case <label> <ssh-prefix-or-empty>
run_case() {
  local label="$1" prefix="${2:-}" pane out col
  echo
  echo "-- $label"

  pane=$("$WEZTERM" cli spawn --new-window -- /bin/bash --norc --noprofile 2>/dev/null)
  sleep 1

  if [ -n "$prefix" ]; then
    send "$pane" "$prefix"$'\n'
    if ! wait_for "$pane" '\$' 15; then
      bad "ssh session established"; return
    fi
    ok "ssh session established"
  fi

  # Unique session name: herdr sessions persist by name, so a fixed one would
  # have the second run attach to leftover state from the first.
  send "$pane" "$HERDR --session spike02-$$-${RANDOM}"$'\n'
  if ! wait_for "$pane" "spaces"; then
    bad "herdr started"
    text "$pane" | sed '/^ *$/d' | head -6 | sed 's/^/       /'
    return
  fi
  ok "herdr started"

  send "$pane" "$PROBE"$'\n'
  if ! wait_for "$pane" "event log"; then
    bad "probe started inside herdr"
    text "$pane" | sed '/^ *$/d' | head -6 | sed 's/^/       /'
    return
  fi

  click "$pane" 40 10
  sleep 0.5
  click "$pane" "$FAR_COL" 12
  sleep 1.5
  out=$(text "$pane")

  col=$(last_down_col "$out")
  if [ -n "$col" ]; then
    ok "herdr forwards mouse events into the pane"
  else
    bad "herdr forwards mouse events into the pane"
    note "requirement 4 (clickable) does not work here"
  fi

  # herdr renders a sidebar, so the pane does not start at screen column 0. A
  # forwarded event must be translated to pane-local coordinates; an
  # untranslated one would arrive equal to the screen column and land on the
  # wrong cell.
  if [ -n "$col" ] && [ "$col" -lt "$FAR_COL" ]; then
    ok "coordinates are pane-local (col $col < screen $FAR_COL)"
  else
    bad "coordinates are pane-local"; note "got col=$col for screen column $FAR_COL"
  fi

  assert_past_x10 "SGR survives past column 223" "$out"

  grep -q "herdr pane   YES" <<<"$out" \
    && ok "HERDR_ENV reaches the pane" \
    || bad "HERDR_ENV reaches the pane"

  # 0.4 -- rendering.
  grep -q "┌─┬─┐" <<<"$out" && ok "box-drawing renders" || bad "box-drawing renders"
  grep -q "resize " <<<"$out" && ok "resize delivered" || bad "resize delivered"

  if [ -n "$prefix" ]; then
    grep -q "over SSH     YES" <<<"$out" \
      && ok "probe confirms it is running over SSH" \
      || bad "probe confirms it is running over SSH"
  fi
}

# ------------------------------------------------------------------ tests ---

echo
echo "=== spike 0.2 / 0.4 — mouse and rendering under herdr ==="
"$WEZTERM" --version
"$HERDR" --version

# Baseline: if injection does not work without herdr, nothing else means anything.
echo
echo "-- baseline: probe alone, no herdr"
base=$("$WEZTERM" cli spawn --new-window -- /bin/bash --norc --noprofile 2>/dev/null)
sleep 1
send "$base" "$PROBE"$'\n'
if wait_for "$base" "event log"; then
  click "$base" 40 10
  click "$base" "$FAR_COL" 12
  sleep 1
  out=$(text "$base")
  [ -n "$(last_down_col "$out")" ] \
    && ok "mouse events reach a plain pane" \
    || { bad "mouse events reach a plain pane"; note "injection is broken"; }
  assert_past_x10 "SGR valid past column 223" "$out"
else
  bad "probe started"
fi

run_case "through herdr (local)" ""

if [ "$MODE" != "local" ]; then
  if start_sshd; then
    run_case "through herdr, over SSH" "$(ssh_cmd)"
  else
    echo
    skip "SSH variant — could not start an unprivileged sshd"
  fi
fi

echo
echo "========================================================"
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
