$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$openSsh = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\OpenSSH' -ErrorAction SilentlyContinue
$shell = if (
    $null -ne $openSsh -and
    -not [string]::IsNullOrWhiteSpace([string]$openSsh.DefaultShell)
) {
    [string]$openSsh.DefaultShell
} else {
    'cmd.exe'
}

function Get-Candidate([string]$Path, [bool]$Sidecar) {
    if ([string]::IsNullOrWhiteSpace($Path) -or -not [IO.File]::Exists($Path)) {
        return $null
    }
    if ($Sidecar) {
        $env:HERDR_REMOTE_SIDECAR_V1 = '1'
        Remove-Item Env:HERDR_ENV -ErrorAction SilentlyContinue
    } else {
        Remove-Item Env:HERDR_REMOTE_SIDECAR_V1 -ErrorAction SilentlyContinue
    }
    $lines = @(& $Path 'status' 'client' '--json')
    if ($LASTEXITCODE -ne 0) {
        return $null
    }
    try {
        $client = (($lines -join "`n").Trim() | ConvertFrom-Json)
    } catch {
        return $null
    }
    $same = (
        [string]$client.version -ceq $Runtime -and
        [uint32]$client.protocol -eq $Protocol
    )
    if ($Sidecar) {
        $a = @($V)
        if ($same -and $null -ne $Hash) {
            $a += $Hash
        }
        & $Path @a | Out-Null
        if ($LASTEXITCODE -ne 0 -and $same -and $null -ne $Hash) {
            & $Path $V | Out-Null
            $same = $false
        }
        if ($LASTEXITCODE -ne 0) {
            return $null
        }
    }
    $eligible = [uint32]$client.endpoint_protocol_generation -eq $Generation
    foreach ($cap in $Caps) {
        $eligible = $eligible -and ($client.endpoint_capabilities -contains $cap)
    }
    if ($Exact) { $eligible = $eligible -and $same }
    $a = @($Session) + @('status', 'server', '--json')
    $lines = @(& $Path @a)
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    try {
        $server = (($lines -join "`n").Trim() | ConvertFrom-Json)
    } catch {
        throw 'invalid Herdr server status JSON'
    }
    return [pscustomobject]@{
        path = $Path
        sidecar = $Sidecar
        matches_current = $same
        eligible = $eligible
        client = $client
        server = $server
    }
}

$path = $null
if ($AllowPath) {
    $cmd = @(Get-Command -Name 'herdr.exe' -CommandType Application -ErrorAction SilentlyContinue)
    if ($cmd.Count -gt 0) {
        $path = Get-Candidate ([string]$cmd[0].Source) $false
    }
}
$sp = [IO.Path]::Combine(
    $env:USERPROFILE,
    '.herdr',
    'remote',
    'herdr.exe'
)
$sidecar = if ($null -eq $path -or -not $path.eligible) {
    Get-Candidate $sp $true
} else {
    $null
}
$candidate = if ($null -ne $path -and $path.eligible) {
    $path
} elseif ($null -ne $sidecar -and $sidecar.eligible) {
    $sidecar
} elseif ($null -ne $path) {
    $path
} else {
    $sidecar
}
[ordered]@{
    os = 'Windows_NT'
    arch = [string]$env:PROCESSOR_ARCHITECTURE
    user_profile = [string]$env:USERPROFILE
    default_shell = $shell
    candidate = $candidate
} | ConvertTo-Json -Compress -Depth 8
exit 0
