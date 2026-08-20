Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-HerdrRemotePlainDirectory {
    param([Parameter(Mandatory = $true)][string] $Path)

    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "unsafe remote sidecar directory: $Path"
    }
}

function Remove-HerdrRemoteFile {
    param(
        [Parameter(Mandatory = $true)][string] $Path,
        [Parameter(Mandatory = $true)][Diagnostics.Stopwatch] $ReleaseWait,
        [ValidateRange(0, 30000)][int] $ReleaseWaitMilliseconds = 0
    )

    while ($true) {
        try {
            [IO.File]::Delete($Path)
            return
        } catch {
            $cause = $_.Exception.GetBaseException()
            $nativeError = $cause.HResult -band 0xffff
            $transientRelease =
                ($cause -is [UnauthorizedAccessException] -or $cause -is [IO.IOException]) -and
                $nativeError -in @(5, 32, 33)
            if (-not $transientRelease -or
                $ReleaseWait.ElapsedMilliseconds -ge $ReleaseWaitMilliseconds) {
                throw
            }
            Start-Sleep -Milliseconds 50
        }
    }
}

function Remove-HerdrRemoteSidecar {
    param(
        [Parameter(Mandatory = $true)][string] $Path,
        [ValidateRange(0, 30000)][int] $ReleaseWaitMilliseconds = 0
    )

    if (-not [IO.Directory]::Exists($Path)) {
        return
    }
    Assert-HerdrRemotePlainDirectory -Path $Path
    $leasePath = Join-Path $Path '.lease'
    if (-not [IO.File]::Exists($leasePath)) {
        throw "remote sidecar is missing its lease: $Path"
    }

    $lease = $null
    $releaseWait = [Diagnostics.Stopwatch]::StartNew()
    while ($null -eq $lease) {
        try {
            $lease = [IO.File]::Open(
                $leasePath,
                [IO.FileMode]::Open,
                [IO.FileAccess]::ReadWrite,
                [IO.FileShare]::None
            )
        } catch [IO.IOException] {
            $nativeError = $_.Exception.HResult -band 0xffff
            if ($nativeError -ne 32 -or
                $releaseWait.ElapsedMilliseconds -ge $ReleaseWaitMilliseconds) {
                throw
            }
            Start-Sleep -Milliseconds 50
        }
    }
    try {
        foreach ($child in Get-ChildItem -LiteralPath $Path -Force) {
            if ($child.Name -eq '.lease') {
                continue
            }
            if ($child.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "unsafe remote sidecar child: $($child.FullName)"
            }
            if ($child.PSIsContainer) {
                Remove-HerdrRemoteTree `
                    -Path $child.FullName `
                    -ReleaseWait $releaseWait `
                    -ReleaseWaitMilliseconds $ReleaseWaitMilliseconds
            } else {
                Remove-HerdrRemoteFile `
                    -Path $child.FullName `
                    -ReleaseWait $releaseWait `
                    -ReleaseWaitMilliseconds $ReleaseWaitMilliseconds
            }
        }
    } finally {
        $lease.Dispose()
    }
    [IO.File]::Delete($leasePath)
    [IO.Directory]::Delete($Path, $false)
}

function Remove-HerdrRemoteTree {
    param(
        [Parameter(Mandatory = $true)][string] $Path,
        [Diagnostics.Stopwatch] $ReleaseWait = $null,
        [ValidateRange(0, 30000)][int] $ReleaseWaitMilliseconds = 0
    )

    if (-not [IO.Directory]::Exists($Path)) {
        return
    }
    if ($null -eq $ReleaseWait) {
        $ReleaseWait = [Diagnostics.Stopwatch]::StartNew()
    }
    Assert-HerdrRemotePlainDirectory -Path $Path
    foreach ($child in Get-ChildItem -LiteralPath $Path -Force) {
        if ($child.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "unsafe remote staging child: $($child.FullName)"
        }
        if ($child.PSIsContainer) {
            Remove-HerdrRemoteTree `
                -Path $child.FullName `
                -ReleaseWait $ReleaseWait `
                -ReleaseWaitMilliseconds $ReleaseWaitMilliseconds
        } else {
            Remove-HerdrRemoteFile `
                -Path $child.FullName `
                -ReleaseWait $ReleaseWait `
                -ReleaseWaitMilliseconds $ReleaseWaitMilliseconds
        }
    }
    [IO.Directory]::Delete($Path, $false)
}

function Assert-HerdrRemoteStagePath {
    param(
        [Parameter(Mandatory = $true)][string] $Stage,
        [Parameter(Mandatory = $true)][string] $Destination
    )

    $parent = Split-Path -Parent $Destination
    $stageItem = Get-Item -LiteralPath $Stage -Force
    if (-not $stageItem.PSIsContainer -or
        ($stageItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $stageItem.Parent.FullName -cne ([IO.DirectoryInfo] $parent).FullName -or
        $stageItem.Name -cnotmatch '^stage-[0-9a-f]{32}$') {
        throw "invalid remote payload stage: $Stage"
    }
}

function Remove-HerdrRemoteStage {
    param(
        [Parameter(Mandatory = $true)][string] $Stage,
        [Parameter(Mandatory = $true)][string] $Destination
    )

    if ([IO.Directory]::Exists($Stage)) {
        Assert-HerdrRemoteStagePath -Stage $Stage -Destination $Destination
        Remove-HerdrRemoteSidecar -Path $Stage
    }
}

function Invoke-HerdrRemoteStageInstall {
    param(
        [Parameter(Mandatory = $true)][string] $Archive,
        [Parameter(Mandatory = $true)][string] $Destination,
        [Parameter(Mandatory = $true)][string] $ExpectedSha256,
        [Parameter(Mandatory = $true)][string] $ExpectedRuntimeVersion,
        [Parameter(Mandatory = $true)][int] $ExpectedProtocol,
        [string] $SessionName = ''
    )

    $parent = Split-Path -Parent $Destination
    $stage = Join-Path $parent ('stage-' + [Guid]::NewGuid().ToString('N'))
    try {
        $actualSha256 = Get-HerdrFileSha256 -Path $Archive
        if ($actualSha256 -ne $ExpectedSha256) {
            throw 'transferred portable payload checksum mismatch'
        }

        Expand-Archive -LiteralPath $Archive -DestinationPath $stage -Force
        Assert-HerdrRemotePlainDirectory -Path $stage
        $exe = Join-Path $stage 'herdr.exe'
        if (-not [IO.File]::Exists($exe)) {
            throw 'portable payload is missing herdr.exe'
        }
        Assert-HerdrPortablePayload -Root $stage

        $status = & $exe status client --json | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0 -or
            [string]$status.version -cne $ExpectedRuntimeVersion -or
            [int]$status.protocol -ne $ExpectedProtocol -or
            [string]::IsNullOrWhiteSpace([string]$status.binary)) {
            throw 'portable payload runtime identity or protocol mismatch'
        }

        $configArguments = @()
        if (-not [string]::IsNullOrEmpty($SessionName)) {
            $configArguments += @('--session', $SessionName)
        }
        $configArguments += @('config', 'check')
        & $exe @configArguments | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw 'remote Herdr configuration is invalid'
        }

        [IO.File]::WriteAllBytes((Join-Path $stage '.lease'), [byte[]]@())
        return $stage
    } catch {
        if (-not [string]::IsNullOrEmpty($stage) -and [IO.Directory]::Exists($stage)) {
            Remove-HerdrRemoteTree -Path $stage
        }
        throw
    } finally {
        if ([IO.File]::Exists($Archive)) {
            [IO.File]::Delete($Archive)
        }
    }
}

function Invoke-HerdrRemoteActivateInstall {
    param(
        [Parameter(Mandatory = $true)][string] $Stage,
        [Parameter(Mandatory = $true)][string] $Destination,
        [string] $ExistingHerdr = '',
        [bool] $ExistingSidecar = $false,
        [string] $SessionName = '',
        [Parameter(Mandatory = $true)][string] $ExpectedRuntimeVersion,
        [Parameter(Mandatory = $true)][int] $ExpectedProtocol
    )

    Assert-HerdrRemoteStagePath -Stage $Stage -Destination $Destination
    if (-not [IO.File]::Exists((Join-Path $Stage '.lease'))) {
        throw 'validated remote payload stage is missing its lease'
    }

    if (-not [string]::IsNullOrEmpty($ExistingHerdr)) {
        if ($ExistingSidecar) {
            $env:HERDR_REMOTE_SIDECAR_V1 = '1'
            Remove-Item Env:HERDR_ENV -ErrorAction SilentlyContinue
        } else {
            Remove-Item Env:HERDR_REMOTE_SIDECAR_V1 -ErrorAction SilentlyContinue
        }
        $scopedArguments = @()
        if (-not [string]::IsNullOrEmpty($SessionName)) {
            $scopedArguments += @('--session', $SessionName)
        }
        $stopArguments = @($scopedArguments) + @('server', 'stop')
        & $ExistingHerdr @stopArguments | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw 'remote server stop failed before payload activation'
        }
    }

    if ([IO.Directory]::Exists($Destination)) {
        Remove-HerdrRemoteSidecar -Path $Destination -ReleaseWaitMilliseconds 10000
    }
    [IO.Directory]::Move($Stage, $Destination)

    $env:HERDR_REMOTE_SIDECAR_V1 = '1'
    Remove-Item Env:HERDR_ENV -ErrorAction SilentlyContinue
    $exe = Join-Path $Destination 'herdr.exe'
    $clientLines = @(& $exe status client --json)
    if ($LASTEXITCODE -ne 0) {
        throw 'activated remote Herdr failed its client status check'
    }
    try {
        $client = (($clientLines -join "`n").Trim() | ConvertFrom-Json)
    } catch {
        throw 'activated remote Herdr returned invalid client status JSON'
    }
    if (
        [string]$client.version -cne $ExpectedRuntimeVersion -or
        [int]$client.protocol -ne $ExpectedProtocol
    ) {
        throw 'activated remote Herdr runtime identity or protocol mismatch'
    }
    $client | ConvertTo-Json -Compress -Depth 8
}
