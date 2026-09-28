#!/usr/bin/env bash
# Orchestrates one browser pass (dev-only; runs on the development host, never in a release):
# it waits for each `=== HANDOFF: ... ===` line the driver prints and answers it, because the
# driver runs in the browser container and cannot reach the demo container's X server itself.
#
#   sh orchestrate.sh <driver-log> <demo-container> <out-dir> <steps-dir>
#
# Each pattern must appear *after* the previous one matched: a log left behind by an earlier run
# would otherwise answer a hand-off the driver has not reached yet (learned the hard way).
#
# The answers, in the order the driver asks:
#   1. start the stand-in app for the input phase (and say so in steps/stand-in-started.txt)
#   2. start it again for the popup phase, then move its popup window far outside its parent
#   3. read the X side of the typing (the stand-in app's key reports and the X client's) into
#      steps/xev-read.txt
#   4. let another X client take the CLIPBOARD selection
#   5. read the CLIPBOARD selection back into steps/host-to-app-read.txt
#   6. change the X cursor
set -u
LOG="$1"; C="$2"; OUT="$3"; STEPS="$4"
XBIN=/xtools/usr/bin
mkdir -p "${STEPS}"
FROM=1

wait_for() { # pattern
  local pattern="$1" tries=0 line
  while [ "${tries}" -lt 900 ]; do
    line=$(tail -n "+${FROM}" "${LOG}" 2>/dev/null | grep -n "${pattern}" | head -1 | cut -d: -f1)
    if [ -n "${line}" ]; then
      FROM=$((FROM + line))
      echo "orchestrate: matched '${pattern}' at offset ${FROM}"
      return 0
    fi
    tries=$((tries + 1))
    sleep 1
  done
  echo "orchestrate: gave up waiting for '${pattern}'" >&2
  return 1
}

write_atomic() { # file, content on stdin
  cat > "$1.tmp"
  mv "$1.tmp" "$1"
}

wait_for "HANDOFF: start the stand-in app now (popup phase)" || exit 1
MSYS_NO_PATHCONV=1 docker exec -d -e DISPLAY=:99 "${C}" bash -c '/target/debug/appricot-spike fake-app --for 180 >/tmp/fakeapp.log 2>&1'
MSYS_NO_PATHCONV=1 docker exec -e DISPLAY=:99 "${C}" bash -c 'sh /tmp/move-popup.sh "1000 620"' || true
echo "orchestrate: popup phase answered"

wait_for "HANDOFF: start the stand-in app now (input observer" || exit 1
printf 'started' | write_atomic "${STEPS}/stand-in-started.txt"
echo "orchestrate: input observer already running (the stand-in app from the popup phase)"

wait_for "HANDOFF: read the X side of the input" || exit 1
MSYS_NO_PATHCONV=1 docker exec "${C}" bash -c "echo '--- the stand-in app (keysym recomputed from the server keymap on every change)'; cat /tmp/fakeapp.log; echo '--- the X client XInputProbe'; grep -E 'KeyPress|KeyRelease' -A 2 ${OUT}/xev.log | grep -o 'keysym 0x[0-9a-f]*, [A-Za-z_0-9]*'" \
  | write_atomic "${STEPS}/xev-read.txt"
echo "orchestrate: key logs written"

wait_for "HANDOFF: set the X clipboard" || exit 1
MSYS_NO_PATHCONV=1 docker exec -d -e DISPLAY=:99 "${C}" bash -c "printf '%s' 'APPricot app-side clipboard text' | ${XBIN}/xclip -selection clipboard"
echo "orchestrate: clipboard taken"

wait_for "HANDOFF: read the X clipboard now" || exit 1
MSYS_NO_PATHCONV=1 docker exec -e DISPLAY=:99 "${C}" bash -c "timeout 15 ${XBIN}/xclip -selection clipboard -o" \
  | write_atomic "${STEPS}/host-to-app-read.txt"
echo "orchestrate: clipboard read back: $(cat "${STEPS}/host-to-app-read.txt")"

wait_for "HANDOFF: change the X cursor now" || exit 1
MSYS_NO_PATHCONV=1 docker exec -e DISPLAY=:99 "${C}" bash -c "${XBIN}/xsetroot -cursor_name crosshair" || true
printf 'changed to crosshair' | write_atomic "${STEPS}/cursor-changed.txt"
echo "orchestrate: cursor changed"

echo "orchestrate: all hand-offs answered"
