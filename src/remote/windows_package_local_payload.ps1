Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$source = [string]$env:HERDR_LOCAL_PAYLOAD_SOURCE
$stage = [string]$env:HERDR_LOCAL_PAYLOAD_STAGE
$archive = [string]$env:HERDR_LOCAL_PAYLOAD_ARCHIVE
foreach ($path in @($source, $stage, $archive)) {
    if ([string]::IsNullOrWhiteSpace($path) -or -not [IO.Path]::IsPathRooted($path)) {
        throw 'local portable payload paths must be absolute'
    }
}
if ([IO.Directory]::Exists($stage) -or [IO.File]::Exists($archive)) {
    throw 'local portable payload staging path already exists'
}

[IO.Directory]::CreateDirectory($stage) | Out-Null
foreach ($relative in $script:HerdrPortablePayloadFiles) {
    $sourcePath = Join-Path $source $relative.Replace('/', '\')
    $sourceItem = Get-Item -LiteralPath $sourcePath -Force
    if (-not $sourceItem.PSIsContainer -and
        -not ($sourceItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        $destination = Join-Path $stage $relative.Replace('/', '\')
        [IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
        [IO.File]::Copy($sourceItem.FullName, $destination, $false)
    } else {
        throw "local portable payload source is not a regular file: $sourcePath"
    }
}

Assert-HerdrPortablePayload -Root $stage
Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::CreateFromDirectory(
    $stage,
    $archive,
    [IO.Compression.CompressionLevel]::Optimal,
    $false
)
