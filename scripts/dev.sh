#!/usr/bin/env bash
# Run a command in the APPricot dev container.
#
#   scripts/dev.sh just check
#   scripts/dev.sh cargo test --workspace --locked
#   scripts/dev.sh bash            (an interactive shell, with Xvfb already up)
#
# A thin wrapper around `docker compose run --rm dev …`, so the container is thrown away and
# only the named volumes survive. It changes nothing about the command it is given.
#
# On Windows, Git Bash rewrites arguments that look like paths (/work -> C:/Program Files/...).
# MSYS_NO_PATHCONV=1 turns that off for this call; it is harmless elsewhere.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ $# -eq 0 ]]; then
    set -- just --list --unsorted
fi

cd "${repo_root}"
MSYS_NO_PATHCONV=1 exec docker compose run --rm dev "$@"
