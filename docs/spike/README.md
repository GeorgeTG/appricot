# The M1 capture spike: the harness

`l1-spike-x11-capture` ([roadmap.md](../roadmap.md#m1-wire-spec-v0-and-the-x11-capture-spike))
measures the pilot application under Xvfb, with the landed streamer as its window manager,
before M2 builds on the result. This page is the method: what runs, how to run it, and what a
run writes. The results are dated pages next to it:

- [findings-2026-09-23.md](findings-2026-09-23.md): start-up, the scene-graph knob, memory, the
  codec, the keyboard check, and capture under a covering window. No device connected yet.

The pilot application is never part of this repository, and never named in it (AGENTS.md, hard
rule). The harness is generic: it runs any X11 program, and it proves itself on `xclock`.

## What runs

One run is one container of the `spike` compose service: the dev image plus the run-time
libraries of a Qt 6 application on the xcb platform plugin
([docker/spike/Dockerfile](../../docker/spike/Dockerfile)),
with the pilot mounted read-only at `/pilot`. On the container's Xvfb (1400x900, depth 24),
[scripts/spike/run.sh](../../scripts/spike/run.sh) starts, in this order:

| # | Process | What it records |
|---|---|---|
| 1 | `appricot-spike observe` | Every window mapped on the root: override-redirect, `WM_TRANSIENT_FOR`, `_NET_WM_WINDOW_TYPE`, `_NET_WM_STATE`, `_MOTIF_WM_HINTS`, class, role, title, geometry; and every Damage rectangle of each window. |
| 2 | `appricot-streamer serve` | The product itself, on loopback:8391, as the window manager from before the application's first window. Release build. |
| 3 | `appricot-spike mem` | Once a second: PSS, RSS and CPU per process group (application, X server, streamer, the spike's tools, other), and the cgroup's anon, shmem and `memory.current`. |
| 4 | the host | **record** mode: `appricot-spike record`, a host that records instead of drawing — the same `Hello`, acks and `ResizeAsk` answers as the demo page, plus snapshots and the decoded tiles. **demo** mode: the demo page, for a person in a browser. |
| 5 | the application | With a knob file's environment, in a scratch working directory. |

The streamer serves one session per process, so demo mode has no recorder: it still has the
inventory, the damage log and the memory samples, but no `wire.md`, no snapshots and no bench.

The tools live in `crates/appricot-spike`. It is a measuring tool, not a product crate: it may
depend on every crate, nothing depends on it, and it is never shipped. Its integration tests run
the observer and the recorder against the real streamer and a stand-in application on the
container's X server, in every `just test`.

## Setup

Build the spike image once (it builds the dev image first):

```sh
docker compose --profile spike build spike
```

Point it at the pilot in the gitignored `.env` at the repository root. Both lines are
machine-local and never committed:

```sh
APPRICOT_PILOT_DIR=<the directory holding the application>
APPRICOT_PILOT_CMD=/pilot/<its executable>
```

Without them `/pilot` is an empty placeholder, and a run names its program after `--`.

## Running

Every run goes through `just spike`, inside the spike container:

```sh
# the harness proving itself on xclock, with its report checked
docker compose --profile spike run --rm spike just spike-selftest

# the pilot's start-up, with a knob file
docker compose --profile spike run --rm spike just spike --label startup \
    --knobs docker/spike/knobs/baseline.env --script scripts/spike/scenarios/startup.spike

# the keyboard check, once per X keyboard layout
docker compose --profile spike run --rm spike just spike --label kbd-gr --xkb gr \
    --script scripts/spike/scenarios/keyboard.spike

# a person drives the application in a browser: the demo page on the one loopback port
docker compose --profile spike run --rm -p "127.0.0.1:${APPRICOT_DEMO_HOST_PORT:-8390}:8390" \
    spike just spike --mode demo --label connected
```

`just spike --help` lists every option. The ones that matter:

| Option | Meaning |
|---|---|
| `--mode record\|demo` | The host: the recorder (default) or the demo page. |
| `--script FILE` | The recorder's input script ([below](#scripts)). |
| `--knobs FILE` | Environment for the application alone ([below](#knobs)). |
| `--xkb LAYOUTS` | The X server's keyboard layouts, as `setxkbmap -layout` takes them. |
| `--codecs raw\|qoi` | What the recorder offers the streamer. |
| `--for SECS` | End the run after this long. |
| `-- CMD ARGS...` | The application, instead of `APPRICOT_PILOT_CMD`. |

Pass `-e APPRICOT_LOG=<filter>` to `docker compose run` to change the streamer's log level;
`info` logs only the first failed input delivery of a session.

## Steering a run

Every tool of a run polls `<run>/control/`. `artifacts/spike/CURRENT` names the newest run, so
from the host, through the bind mount:

```sh
run="$(cat artifacts/spike/CURRENT)"
echo login > "$run/control/mark"          # the scenario label from now on
touch "$run/control/snap-after-login"     # one PNG per live surface (record mode)
touch "$run/control/stop"                 # every tool writes its summary and exits
```

Every log line, damage rectangle, sample and tile carries the label in force when it was
written, so each summary table has one row per scenario.

## What a run writes

`artifacts/spike/<UTC>-<label>/`, gitignored:

| File | Contents |
|---|---|
| `report.md` | All the summaries below in one file, then the first lines of `app.log`. |
| `meta.md` | Date, mode, script, knobs, keyboard, application, host, X server, and whether `ldd` found every library. |
| `inventory.md`, `inventory.jsonl` | The window table and damage per scenario; every create, map, unmap, configure and property change. |
| `damage.jsonl` | Every Damage rectangle. |
| `wire.md`, `wire.jsonl` | Frames, tiles, bytes and codecs per scenario; every message the streamer sent. |
| `snapshots/*.png` | One PNG per live surface for each `snap`, and a `final` set at the end: what a host would draw. |
| `tiles.bin`, `bench.md` | Every tile decoded, and the codec bench over them. |
| `mem.md`, `mem.jsonl` | Memory and CPU per scenario and group, and the cgroup. |
| `app.log`, `streamer.log`, … | Each process's own output. |

The cgroup figure to compare with a reference is **anon + shmem**, the memory the session's
processes own. `memory.current` adds the page cache, which the release build just before the run
fills.

## Scripts

A script ([scripts/spike/scenarios/](../../scripts/spike/scenarios/)) drives the application
through the streamer exactly as a browser client does: `FocusNotify`, pointer and `Key`
messages, which the streamer turns into XTEST input. One command per line; the grammar is the
module documentation of `crates/appricot-spike/src/script.rs`. `wait-for toplevel N` waits for
a window rather than for time, which keeps a script deterministic.

| Script | What it does |
|---|---|
| `selftest.spike` | The harness's own check on `xclock`. |
| `startup.spike` | Start-up, then a minute idle: the inventory before anyone acts, idle damage, memory at rest. |
| `keyboard.spike` | Latin, Greek, accented Greek and AltGr text into the focused field, each group snapped. |

A script that clicks the pilot's own widgets names their coordinates, which describe that
application. Such scripts stay machine-local, under `artifacts/spike/scenarios/`.

## Knobs

A knob file ([docker/spike/knobs/](../../docker/spike/knobs/)) is environment for the
application alone. The roadmap's rule holds: a knob goes into an app profile only with a
recorded visual check, per profile. A run's snapshots are that check. Compare them with the
baseline's, pixel by pixel, not by eye alone.

| File | Sets |
|---|---|
| `baseline.env` | `QSG_INFO=1`: Qt Quick logs the scene-graph backend it chose. Nothing else. |
| `software-sg.env` | `QT_QUICK_BACKEND=software` as well: the raster scene graph instead of OpenGL on Mesa's llvmpipe. |

## Writing the results down

A findings page is dated, names the method (`just spike`, the scripts and knobs used) and the
host, and follows the naming rule: "the pilot application", never its name, its vendor, or a
string from its log. Measurements from this harness are reproducible from this repository;
figures from the earlier internal benchmark stay attributed to it.
