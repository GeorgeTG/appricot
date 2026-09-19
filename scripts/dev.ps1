# Run a command in the APPricot dev container.
#
#   .\scripts\dev.ps1 just check
#   .\scripts\dev.ps1 cargo test --workspace --locked
#   .\scripts\dev.ps1 bash -c "cargo fmt --all"
#   .\scripts\dev.ps1 bash            # an interactive shell, with Xvfb already up
#
# A thin wrapper around `docker compose run --rm dev ...`, so the container is thrown away and
# only the named volumes survive. It changes nothing about the command it is given, and it
# exits with that command's exit code.
#
# There is deliberately NO param() block. With one, PowerShell binds any argument that starts
# with a dash to a parameter of this script by prefix: `.\scripts\dev.ps1 bash -c "exit 7"`
# failed with "A positional parameter cannot be found that accepts argument 'bash'", because
# `-c` was read as an abbreviation of `-Command` (measured 2026-09-19). $args takes every
# argument as written.

$ErrorActionPreference = 'Stop'

$command = $args
if ($command.Count -eq 0) {
    $command = @('just', '--list', '--unsorted')
}

$repoRoot = Split-Path -Parent $PSScriptRoot

Push-Location $repoRoot
try {
    & docker compose run --rm dev @command
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}
