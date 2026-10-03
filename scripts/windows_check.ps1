param(
    [ValidateSet("lint", "check")]
    [string]$Mode = "check"
)

$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param([string]$Command, [string[]]$Arguments)

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $Command $($Arguments -join ' ')"
    }
}

Invoke-Checked cargo @("fmt", "--check")
Invoke-Checked cargo @(
    "clippy",
    "--all-targets",
    "--locked",
    "--jobs", [string][Environment]::ProcessorCount,
    "--",
    "-D",
    "warnings"
)

if ($Mode -eq "lint") {
    return
}

$env:CARGO_BUILD_JOBS = [string][Environment]::ProcessorCount
Invoke-Checked just @("test")
Invoke-Checked cargo @("build", "--locked", "--jobs", [string][Environment]::ProcessorCount)
