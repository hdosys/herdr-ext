param(
    [Parameter(Mandatory = $true)]
    [string]$ArchivePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$archive = (Resolve-Path -LiteralPath $ArchivePath).Path
$tempRoot = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    [System.IO.Path]::GetTempPath()
} else {
    $env:RUNNER_TEMP
}
$root = Join-Path $tempRoot ("herdr-conpty-archive-test-" + [Guid]::NewGuid().ToString("N"))
$stage = Join-Path $root "stage"
New-Item -ItemType Directory -Path $root | Out-Null
try {
    Expand-Archive -LiteralPath $archive -DestinationPath $stage
    & python "$PSScriptRoot\package_windows_conpty.py" "validate" "--stage-dir" $stage
    if ($LASTEXITCODE -ne 0) {
        throw "ConPTY archive validation failed with exit code $LASTEXITCODE"
    }
    # Validate the executable's acceptance of the adjacent bundle, not only
    # metadata versus payload bytes. The existing smoke creates and uses a PTY.
    $savedConpty = $env:HERDR_WINDOWS_CONPTY
    try {
        Remove-Item Env:HERDR_WINDOWS_CONPTY -ErrorAction SilentlyContinue
        & "$PSScriptRoot\windows_smoke_conpty_path.ps1" -ExePath (Join-Path $stage 'herdr.exe')
    } finally {
        if ($null -eq $savedConpty) {
            Remove-Item Env:HERDR_WINDOWS_CONPTY -ErrorAction SilentlyContinue
        } else {
            $env:HERDR_WINDOWS_CONPTY = $savedConpty
        }
    }
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
