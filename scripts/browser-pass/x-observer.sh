#!/bin/sh
# X-side observer for the M2 browser pass (dev-only; runs in the demo container, never on the
# host). Read-only: it lists windows and reads xev's log; it never moves or resizes anything.
#
#   $1  output directory (default /work/artifacts/browser-pass/out — the repo is bind-mounted
#       at /work, so the logs outlive the throwaway demo container)
#
#   x-observer.log  one block per 300 ms: input focus + every root child with geometry
#   x-keys.log      xev's key events, each line tagged with a UTC timestamp
#
# xev writes into the same directory; this tails it so the key events carry the same clock as
# the browser driver's timeline.
set -u
out="${1:-/work/artifacts/browser-pass/out}"
mkdir -p "${out}"
: >"${out}/x-observer.log"
: >"${out}/x-keys.log"

( tail -Fn0 "${out}/xev.log" 2>/dev/null | while IFS= read -r line; do
    printf 'KEY %s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)" "$line"
  done ) >>"${out}/x-keys.log" 2>&1 &

while true; do
  ts=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)
  focused=$(xdotool getwindowfocus 2>/dev/null || echo none)
  focused_name=$(xdotool getwindowfocus getwindowname 2>/dev/null || echo none)
  printf '### %s focus=%s name=%s\n' "${ts}" "${focused}" "${focused_name}"
  xwininfo -root -children 2>/dev/null | sed -n '/children:/,$p' | tail -n +2 | sed 's/^ *//'
  sleep 0.3
done >>"${out}/x-observer.log" 2>&1
