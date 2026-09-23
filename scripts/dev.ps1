# Run a command in the APPricot dev container.
#
#   .\scripts\dev.ps1 just check
#   .\scripts\dev.ps1 cargo test --workspace --locked
#   .\scripts\dev.ps1 cargo test --workspace -- --nocapture
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
#
# With one exception: PowerShell's parameter binder swallows the first bare `--`, even with no
# param() block, so `cargo test -- --nocapture` reached $args as `cargo test --nocapture`
# (measured in PowerShell 7.6.6, 2026-09-23). This script puts it back: it parses its own
# invocation, finds where the `--` stood, and checks that the arguments after it are the ones
# in $args before inserting it. When that check cannot be made, it stops and says so rather
# than run a different command. A quoted '--' is never swallowed, and `pwsh -File` passes a
# bare one through unchanged.

using namespace System.Management.Automation.Language

$ErrorActionPreference = 'Stop'

$command = [System.Collections.Generic.List[object]]::new()
foreach ($arg in $args) { $command.Add($arg) }

# The text of the statement that invoked this script. Statement (PowerShell 7.4+) spans
# continuation lines; Line is the first line only, and is empty under `pwsh -File`.
$invocationText = $MyInvocation.Statement
$column = $MyInvocation.OffsetInLine
if ($invocationText) {
    $firstLine = ($invocationText -split '\r?\n')[0]
    $start = if ($MyInvocation.Line) { $MyInvocation.Line.IndexOf($firstLine) } else { -1 }
    if ($start -ge 0) { $column -= $start } else { $invocationText = $null }
}
if (-not $invocationText) { $invocationText = $MyInvocation.Line }

if ($invocationText) {
    $ast = [Parser]::ParseInput($invocationText, [ref]$null, [ref]$null)
    $call = $ast.Find({
            param($node)
            $node -is [CommandAst] -and
            $node.Extent.StartLineNumber -eq 1 -and $node.Extent.StartColumnNumber -eq $column
        }, $true)
    if ($call) {
        $elements = @($call.CommandElements | Select-Object -Skip 1)
        $cut = -1
        for ($i = 0; $i -lt $elements.Count; $i++) {
            if ($elements[$i] -is [CommandParameterAst] -and $elements[$i].Extent.Text -eq '--') {
                $cut = $i
                break
            }
        }
        if ($cut -ge 0) {
            # Everything after the `--` is positional, one element per argument, unless it is
            # splatted. Constant elements must match the tail of $args exactly.
            $tail = @($elements | Select-Object -Skip ($cut + 1))
            $at = $command.Count - $tail.Count
            $safe = $at -ge 0
            for ($k = 0; $safe -and $k -lt $tail.Count; $k++) {
                $element = $tail[$k]
                if ($element -is [VariableExpressionAst] -and $element.Splatted) {
                    $safe = $false
                }
                elseif ($element -is [StringConstantExpressionAst] -and
                    $element.Value -cne [string]$command[$at + $k]) {
                    $safe = $false
                }
            }
            if (-not $safe) {
                [Console]::Error.WriteLine(
                    "dev.ps1: PowerShell swallowed a bare '--' and it cannot be put back " +
                    "safely here. Quote it as '--' and run the command again.")
                exit 2
            }
            $command.Insert($at, '--')
        }
    }
}

if ($command.Count -eq 0) {
    $command.AddRange([object[]]@('just', '--list', '--unsorted'))
}

$repoRoot = Split-Path -Parent $PSScriptRoot

Push-Location $repoRoot
try {
    $argv = $command.ToArray()
    & docker compose run --rm dev @argv
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}
