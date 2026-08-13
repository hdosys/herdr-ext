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
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
