#!/usr/bin/env bash
# One run of the M1 capture spike (docs/spike/README.md). It runs INSIDE the spike container:
#
#   docker compose --profile spike run --rm spike just spike [OPTIONS] [-- CMD ARGS...]
#
# On this container's Xvfb it starts, in this order:
#
#   1. the observer   (appricot-spike observe): every window mapped, its properties, its damage;
#   2. the streamer   (appricot-streamer serve, on loopback:8391): the window manager, as in
#                     production, from before the application's first window;
#   3. the sampler    (appricot-spike mem): PSS/RSS/CPU per process group, cgroup memory;
#   4. the host       record mode: the recorder (appricot-spike record), driven by a script;
#                     demo mode: the demo page, for a person in a browser;
#   5. the application, with the knob file's environment.
#
# Options:
#
#   --label NAME        names the run directory (default: the application's file name)
#   --mode record|demo  the host: the recorder (default) or the demo page. The streamer serves
#                       one session, so demo mode has no recorder: no wire.md and no snapshots.
#   --script FILE       the recorder's input script (scripts/spike/scenarios/*.spike)
#   --knobs FILE        environment for the application alone (docker/spike/knobs/*.env)
#   --xkb LAYOUTS       the X server's keyboard layouts before anything starts, as
#                       setxkbmap takes them: `gr`, or `us,gr` (default: the server's, `us`)
#   --codecs raw|qoi    what the recorder offers (default qoi)
#   --for SECS          end the run after this long. Default: in record mode, 60 s without a
#                       script and 600 s with one (the script's `stop` ends it first); in demo
#                       mode, no limit.
#   --snap-every SECS   a snapshot of every live surface this often (record mode)
#   --app-prefix PATH   which processes count as the application in mem.md (default: /pilot/
#                       for an application there, else the executable's own path)
#   -- CMD ARGS...      the application. Default: $APPRICOT_PILOT_CMD.
#
# The run directory is /work/artifacts/spike/<UTC>-<label>/ (artifacts/ is gitignored), and
# artifacts/spike/CURRENT names it. A person on the host steers a run through its control files:
#
#   echo login > artifacts/spike/<run>/control/mark        # the scenario label from now on
#   touch artifacts/spike/<run>/control/snap-after-login   # one PNG per live surface
#   touch artifacts/spike/<run>/control/stop               # end the run
#
# At the end the run directory holds report.md: meta.md, inventory.md, wire.md, mem.md and
# bench.md in one file. The exit status is the recorder's (a failed script step fails the run),
# or 0 in demo mode.
set -euo pipefail

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//' >&2
    exit 2
}

label="" mode=record script="" knobs="" xkb="" codecs=qoi duration="" snap_every="" app_prefix=""
app=()
while (($#)); do
    case "$1" in
        --label) label="${2:?--label needs a name}"; shift 2 ;;
        --mode) mode="${2:?--mode needs record or demo}"; shift 2 ;;
        --script) script="${2:?--script needs a file}"; shift 2 ;;
        --knobs) knobs="${2:?--knobs needs a file}"; shift 2 ;;
        --xkb) xkb="${2:?--xkb needs a layout list}"; shift 2 ;;
        --codecs) codecs="${2:?--codecs needs raw or qoi}"; shift 2 ;;
        --for) duration="${2:?--for needs seconds}"; shift 2 ;;
        --snap-every) snap_every="${2:?--snap-every needs seconds}"; shift 2 ;;
        --app-prefix) app_prefix="${2:?--app-prefix needs a path}"; shift 2 ;;
        --) shift; app=("$@"); break ;;
        -h|--help) usage ;;
        *) echo "spike: unknown option '$1' (--help lists them)" >&2; exit 2 ;;
    esac
done

fail() { echo "spike: $*" >&2; exit 1; }

[[ "${mode}" == record || "${mode}" == demo ]] || fail "--mode is record or demo, not '${mode}'"
[[ -z "${script}" || -f "${script}" ]] || fail "no script at ${script}"
[[ -z "${knobs}" || -f "${knobs}" ]] || fail "no knob file at ${knobs}"
[[ -n "${DISPLAY:-}" ]] || fail "DISPLAY is not set; run inside the spike container"
# Absolute from here on: the application starts in a directory of its own.
[[ -z "${script}" ]] || script="$(realpath -- "${script}")"
[[ -z "${knobs}" ]] || knobs="$(realpath -- "${knobs}")"

if ((${#app[@]} == 0)); then
    [[ -n "${APPRICOT_PILOT_CMD:-}" ]] \
        || fail "no application: pass '-- CMD ARGS...' or set APPRICOT_PILOT_CMD in .env"
    read -r -a app <<<"${APPRICOT_PILOT_CMD}"
fi

# --- build -----------------------------------------------------------------------------------
# Release builds: the streamer's CPU and memory are part of what the spike measures.
cargo build --release --locked -p appricot-spike -p appricot-streamer
bin="${CARGO_TARGET_DIR:-/work/target}/release"
export PATH="${bin}:${PATH}"

# The executable as an absolute path: the sampler tells the application's processes apart by
# the first word of their command line.
exe="$(command -v -- "${app[0]}")" || fail "cannot find the application '${app[0]}'"
app[0]="${exe}"
if [[ -z "${app_prefix}" ]]; then
    if [[ "${exe}" == /pilot/* ]]; then app_prefix=/pilot/; else app_prefix="${exe}"; fi
fi
[[ -n "${label}" ]] || label="$(basename -- "${exe}")"

# --- the run directory -----------------------------------------------------------------------
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
label="$(printf '%s' "${label}" | tr -c 'A-Za-z0-9._-' '_' | cut -c1-48)"
name="${stamp}-${label}"
run="/work/artifacts/spike/${name}"
mkdir -p "${run}/control"
printf 'artifacts/spike/%s\n' "${name}" >/work/artifacts/spike/CURRENT
control="${run}/control"
[[ -n "${knobs}" ]] && cp -- "${knobs}" "${run}/knobs.env"
[[ -n "${script}" ]] && cp -- "${script}" "${run}/script.spike"

# The keymap first: the streamer reads it at start (and again on every MappingNotify), and
# the application reads it when it connects.
if [[ -n "${xkb}" ]]; then
    setxkbmap -layout "${xkb}" || fail "setxkbmap refused the layouts '${xkb}'"
fi

# 24 random bytes as hex, fresh for the run. It is never written into the run directory.
token="$(head -c 24 /dev/urandom | od -An -vtx1 | tr -d ' \n')"

# --- meta.md ---------------------------------------------------------------------------------
{
    echo "# Spike run ${name}"
    echo
    echo "| | |"
    echo "|---|---|"
    echo "| Started (UTC) | $(date -u '+%Y-%m-%d %H:%M:%S') |"
    echo "| Mode | ${mode} |"
    echo "| Codecs offered | ${codecs} |"
    echo "| Script | ${script:-none} |"
    echo "| Knob file | ${knobs:-none} |"
    echo "| Keyboard | $(setxkbmap -query | awk '/^(model|layout|variant|options):/ {printf "%s %s; ", $1, $2}') |"
    echo "| Application | \`${app[*]}\` |"
    echo "| Counted as the application | processes whose command starts with \`${app_prefix}\` |"
    echo "| Kernel, CPUs, memory | $(uname -r), $(nproc) CPUs, $(awk '/^MemTotal/ {printf "%.1f GiB", $2/1048576}' /proc/meminfo) |"
    echo "| cgroup memory.max | $(cat /sys/fs/cgroup/memory.max 2>/dev/null || echo unknown) |"
    echo "| Debian | $(cat /etc/debian_version) |"
    echo "| Streamer | $("${bin}/appricot-streamer" 2>&1 | head -n1) |"
    echo "| X server | $(xdpyinfo | awk -F': *' '/^vendor string|^vendor release/ {printf "%s ", $2}') |"
    echo "| Root | $(xdpyinfo | awk '/dimensions:/ {print $2}'), depth $(xdpyinfo | awk '/depth of root window:/ {print $5}') |"
    echo
    if [[ -n "${knobs}" ]]; then
        echo "## Knobs"
        echo
        echo '```'
        grep -v '^[[:space:]]*#' -- "${knobs}" | sed '/^[[:space:]]*$/d'
        echo '```'
        echo
    fi
    echo "## Shared libraries"
    echo
    missing="$(ldd -- "${exe}" 2>&1 | grep 'not found' || true)"
    if [[ -n "${missing}" ]]; then
        echo "**Missing** (the application will likely not start):"
        echo
        echo '```'
        echo "${missing}"
        echo '```'
    elif ldd -- "${exe}" >/dev/null 2>&1; then
        echo "Every library \`ldd\` lists is found ($(ldd -- "${exe}" | wc -l) entries)."
    else
        echo "\`ldd\` does not apply to this executable (a script, or statically linked)."
    fi
    echo
} >"${run}/meta.md"

# --- processes -------------------------------------------------------------------------------
observer="" streamer="" sampler="" host="" app_pid="" demo=""
finished=0
cleanup() {
    ((finished)) && return
    finished=1
    touch "${control}/stop"
    # The tools poll the stop file (100 ms; the sampler at its interval) and write summaries.
    for pid in ${host} ${observer} ${sampler}; do
        for _ in $(seq 1 100); do kill -0 "${pid}" 2>/dev/null || break; sleep 0.1; done
        kill "${pid}" 2>/dev/null || true
    done
    if [[ -n "${app_pid}" ]] && kill -0 "${app_pid}" 2>/dev/null; then
        kill "${app_pid}" 2>/dev/null || true
        for _ in $(seq 1 50); do kill -0 "${app_pid}" 2>/dev/null || break; sleep 0.1; done
        kill -9 "${app_pid}" 2>/dev/null || true
    fi
    kill ${demo} ${streamer} 2>/dev/null || true
    wait 2>/dev/null || true
    report
}

report() {
    if [[ -f "${run}/tiles.bin" ]]; then
        "${bin}/appricot-spike" bench "${run}" >/dev/null 2>"${run}/bench.log" \
            || echo "spike: the bench failed; see ${run}/bench.log" >&2
    fi
    {
        cat "${run}/meta.md"
        for part in inventory wire mem bench; do
            if [[ -f "${run}/${part}.md" ]]; then
                cat "${run}/${part}.md"
                echo
            fi
        done
        echo "## Application log"
        echo
        echo "The first 40 lines of app.log:"
        echo
        echo '```'
        head -n 40 "${run}/app.log" 2>/dev/null || true
        echo '```'
    } >"${run}/report.md"
    echo "spike: run ${name} done: artifacts/spike/${name}/report.md" >&2
}
trap cleanup EXIT
trap 'exit 130' INT TERM

wait_for() { # what, seconds, command...
    local what="$1" tries=$(($2 * 10))
    shift 2
    for _ in $(seq 1 "${tries}"); do
        if "$@"; then return 0; fi
        sleep 0.1
    done
    fail "${what} did not happen within $((tries / 10)) s"
}

"${bin}/appricot-spike" observe --out "${run}" >"${run}/observe.log" 2>&1 &
observer=$!
wait_for "the observer's start" 10 grep -qs '"ev":"start"' "${run}/inventory.jsonl"

APPRICOT_BIND=loopback:8391 APPRICOT_STREAM_TOKEN="${token}" \
    "${bin}/appricot-streamer" serve >"${run}/streamer.log" 2>&1 &
streamer=$!
wait_for "the streamer's readiness" 30 curl -fsS -o /dev/null http://127.0.0.1:8391/readyz

"${bin}/appricot-spike" mem --out "${run}" --app-prefix "${app_prefix}" >"${run}/mem.log" 2>&1 &
sampler=$!

if [[ "${mode}" == record ]]; then
    if [[ -z "${duration}" ]]; then
        if [[ -n "${script}" ]]; then duration=600; else duration=60; fi
    fi
    args=(record --out "${run}" --url ws://127.0.0.1:8391/session --tiles --codecs "${codecs}"
        --for "${duration}")
    [[ -n "${script}" ]] && args+=(--script "${script}")
    [[ -n "${snap_every}" ]] && args+=(--snap-every "${snap_every}")
    APPRICOT_STREAM_TOKEN="${token}" "${bin}/appricot-spike" "${args[@]}" \
        >"${run}/record.log" 2>&1 &
    host=$!
    # The session is open before the application starts, so its first windows are on the wire.
    wait_for "the recorder's session" 30 grep -qs '"ev":"hello_reply"' "${run}/wire.jsonl"
else
    pnpm build >/dev/null
    node packages/demo/server.mjs >"${run}/demo.log" 2>&1 &
    demo=$!
    wait_for "the demo page" 30 curl -fsS -o /dev/null http://127.0.0.1:8390/
    echo "spike: the demo page is up: http://127.0.0.1:8390/ on the host" \
        "(or the port APPRICOT_DEMO_HOST_PORT names)" >&2
    echo "spike: stream token (paste it into the page): ${token}" >&2
fi

# The application: the knob file's environment on top of this one, in a scratch working
# directory so that nothing it writes lands in the repository.
mkdir -p "${HOME}/spike-cwd"
(
    cd "${HOME}/spike-cwd"
    if [[ -n "${knobs}" ]]; then set -a; . "${knobs}"; set +a; fi
    exec "${app[@]}"
) >"${run}/app.log" 2>&1 &
app_pid=$!
echo "spike: run ${name} (${mode}); stop it with: touch artifacts/spike/${name}/control/stop" >&2

# Until the host is done (record), or until stopped (demo). The application ending early ends
# the run, which still reports what it saw; a non-zero exit of the application fails it.
status=0
app_status=0
started=${SECONDS}
while :; do
    if [[ -n "${host}" ]] && ! kill -0 "${host}" 2>/dev/null; then
        wait "${host}" || status=$?
        host=""
        break
    fi
    if [[ -f "${control}/stop" ]]; then break; fi
    if [[ -n "${duration}" && "${mode}" == demo ]] && ((SECONDS - started >= duration)); then
        break
    fi
    if [[ -n "${app_pid}" ]] && ! kill -0 "${app_pid}" 2>/dev/null; then
        wait "${app_pid}" || app_status=$?
        echo "spike: the application exited (status ${app_status}) before the run ended" >&2
        echo "The application exited with status ${app_status} before the run ended." \
            >>"${run}/meta.md"
        app_pid=""
        touch "${control}/stop"
    fi
    sleep 0.2
done
if ((status != 0)); then
    echo "spike: the recorder failed (status ${status}); see ${run}/record.log" >&2
elif ((app_status != 0)); then
    echo "spike: the application failed (status ${app_status}); see ${run}/app.log" >&2
    status=1
fi
exit "${status}"
