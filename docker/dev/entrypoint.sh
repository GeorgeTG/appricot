#!/usr/bin/env bash
# Dev container entrypoint: start a headless X server, wait until it answers, then run the
# command.
#
#   docker compose run --rm dev <cmd...>
#
# crates/appricot-x11/tests/extensions.rs needs an X server with Composite, Damage, XTEST and
# XFixes on $DISPLAY, and it fails (never skips) without one. So every container gets one.
#
# -nolisten tcp: the server is reachable only through its unix socket in /tmp/.X11-unix, inside
# this container. No -auth file: the only clients are this container's own processes.
#
# -noreset: Xvfb resets its state when its LAST client disconnects (the man page's RESET section).
# `cargo test --workspace` runs several test binaries back to back in this one container, each an
# X client; between two binaries there is a moment with no client at all, and a reset there wipes
# the window manager's state out from under the next binary's tests (measured by the wave-1 X11
# backend work, 2026-09-20). -noreset keeps the server - and the backend's ownership of the root
# - alive across that gap.
#
# Two levers, and both are honoured rather than silently overridden:
#
#   -e APPRICOT_XVFB=0    no X server at all. DISPLAY is left exactly as it arrived, which
#                         for a plain `docker compose run` means unset. That is how you prove
#                         the X11 test fails loudly instead of skipping; `just display-levers`
#                         proves it, and both levers, on every `just check`.
#   -e DISPLAY=<value>    the caller names the display. No Xvfb is started and DISPLAY is used
#                         as given, empty included. Passing an empty DISPLAY is the same as
#                         asking for no X server.
#
# Nothing in the image or in compose.yml sets DISPLAY, so `${DISPLAY+set}` below is non-empty
# only when a caller passed one. Before 2026-09-19 this script exported its own DISPLAY over
# whatever the caller had asked for, so `-e DISPLAY=` looked like it did nothing: the test
# still found :99 and passed.
set -euo pipefail

if [[ "${APPRICOT_XVFB:-1}" == "0" ]]; then
    : # No X server. DISPLAY is untouched.
elif [[ -n "${DISPLAY+set}" ]]; then
    echo "appricot-entrypoint: DISPLAY='${DISPLAY}' came from the caller; not starting Xvfb." >&2
else
    display="${APPRICOT_XVFB_DISPLAY:-:99}"
    screen="${APPRICOT_XVFB_SCREEN:-1400x900x24}"
    log="/tmp/xvfb${display#:}.log"

    Xvfb "${display}" -screen 0 "${screen}" -nolisten tcp -noreset >"${log}" 2>&1 &
    xvfb_pid=$!

    # Poll for readiness: xdpyinfo exits 0 only once the server accepts a connection.
    # 100 tries x 0.1 s = 10 s, far above the ~0.2 s Xvfb takes.
    ready=0
    for _ in $(seq 1 100); do
        if ! kill -0 "${xvfb_pid}" 2>/dev/null; then
            echo "appricot-entrypoint: Xvfb ${display} exited during startup:" >&2
            cat "${log}" >&2
            exit 1
        fi
        if xdpyinfo -display "${display}" >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 0.1
    done
    if [[ "${ready}" != "1" ]]; then
        echo "appricot-entrypoint: Xvfb ${display} did not answer within 10 s:" >&2
        cat "${log}" >&2
        exit 1
    fi

    export DISPLAY="${display}"
fi

exec "$@"
