# No pilot application

This directory is what the `spike` service mounts at `/pilot` when `APPRICOT_PILOT_DIR` is not
set. It holds no application, on purpose: the pilot is never part of this repository.

Point `APPRICOT_PILOT_DIR` at a directory holding the application, in the gitignored `.env` at
the repository root, and name the executable in `APPRICOT_PILOT_CMD` — see
[docs/spike/README.md](../../../docs/spike/README.md). Without them, the harness still runs any
other X11 program: `just spike -- xclock -update 1`.
