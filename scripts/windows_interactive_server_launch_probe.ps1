[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $ExePath,

    [ValidateRange(10, 120)]
    [int] $TimeoutSeconds = 45
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

function Get-ExactRuntimeProcesses {
    param([string] $RuntimePath)

    $target = [IO.Path]::GetFullPath($RuntimePath)
    @(
        Get-CimInstance -ClassName Win32_Process -Filter "Name = 'herdr.exe'" |
            Where-Object {
                -not [string]::IsNullOrWhiteSpace($_.ExecutablePath) -and
                [string]::Equals(
                    [IO.Path]::GetFullPath($_.ExecutablePath),
                    $target,
                    [StringComparison]::OrdinalIgnoreCase
                )
            }
    )
}

function Get-InteractiveTaskNames {
    $service = New-Object -ComObject 'Schedule.Service'
    $service.Connect()
    $tasks = $service.GetFolder('\').GetTasks(1)
    $names = @()
    for ($index = 1; $index -le $tasks.Count; $index++) {
        $task = $tasks.Item($index)
        if ($task.Name -like 'HerdrInteractiveServer-*') {
            $names += $task.Name
        }
    }
    @($names | Sort-Object)
}

function Get-InteractiveLaunchDirectories {
    param([string] $Base)

    if (-not [IO.Directory]::Exists($Base)) {
        return @()
    }
    @(
        [IO.Directory]::EnumerateDirectories($Base) |
            Where-Object {
                [IO.Path]::GetFileName($_).StartsWith(
                    'herdr-server-launch-',
                    [StringComparison]::OrdinalIgnoreCase
                )
            } |
            Sort-Object
    )
}

function Stop-OwnedRuntimeProcesses {
    param(
        [string] $RuntimePath,
        [int[]] $InitialProcessIds
    )

    $owned = @(
        Get-ExactRuntimeProcesses $RuntimePath |
            Where-Object { $InitialProcessIds -notcontains [int] $_.ProcessId }
    )
    foreach ($process in $owned) {
        & "$env:SystemRoot\System32\taskkill.exe" /PID $process.ProcessId /T /F 2>&1 | Out-Null
    }
}

$stopwatch = [Diagnostics.Stopwatch]::StartNew()
$exe = (Resolve-Path -LiteralPath $ExePath).Path
if ([IO.Path]::GetFileName($exe) -ine 'herdr.exe') {
    throw "interactive server probe requires herdr.exe, got $exe"
}

$profile = [Environment]::GetFolderPath('UserProfile')
$bootstrapBase = Join-Path $profile 'AppData\Local\herdr\server-launch'
$createdBootstrapBase = $false
if ([IO.Directory]::Exists($bootstrapBase)) {
    $bootstrapItem = Get-Item -LiteralPath $bootstrapBase -Force
    if (($bootstrapItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "interactive server launch root must not be a reparse point: $bootstrapBase"
    }
} else {
    [IO.Directory]::CreateDirectory($bootstrapBase) | Out-Null
    $createdBootstrapBase = $true
}

$initialTasks = @(Get-InteractiveTaskNames)
if ($initialTasks.Count -ne 0) {
    throw "interactive server probe found pre-existing scheduled task residue: $($initialTasks -join ', ')"
}
$initialLaunchDirectories = @(Get-InteractiveLaunchDirectories $bootstrapBase)
if ($initialLaunchDirectories.Count -ne 0) {
    throw "interactive server probe found pre-existing bootstrap residue: $($initialLaunchDirectories -join ', ')"
}
$initialProcesses = @(Get-ExactRuntimeProcesses $exe)
$initialProcessIds = @($initialProcesses | ForEach-Object { [int] $_.ProcessId })

$probeId = $null
$temporary = $null
for ($attempt = 0; $attempt -lt 10; $attempt++) {
    $candidateId = [Guid]::NewGuid().ToString('N').Substring(0, 8)
    $candidateRoot = Join-Path ([IO.Path]::GetTempPath()) "h$candidateId"
    if (-not [IO.Directory]::Exists($candidateRoot)) {
        $probeId = $candidateId
        $temporary = $candidateRoot
        break
    }
}
if ($null -eq $temporary) {
    throw 'could not allocate a unique interactive server probe root'
}
[IO.Directory]::CreateDirectory($temporary) | Out-Null
$batch = Join-Path $temporary 'launch.cmd'
[IO.File]::WriteAllLines(
    $batch,
    @(
        '@echo off',
        'set "HERDR_REMOTE_SIDECAR_V1="',
        'set "HERDR_BIN_PATH="',
        'set "HERDR_SANDBOX_HERDR_EXE="',
        'cd /d "%HERDR_PROBE_WORKING_DIRECTORY%"',
        '"%HERDR_PROBE_EXE%" server start',
        'exit /b %ERRORLEVEL%'
    ),
    [Text.Encoding]::ASCII
)

$savedEnvironment = @{}
$environmentNames = @(
    Get-ChildItem Env: |
        Where-Object { $_.Name -like 'HERDR_*' } |
        ForEach-Object Name
    'XDG_CONFIG_HOME'
    'XDG_STATE_HOME'
) | Sort-Object -Unique
foreach ($name in $environmentNames) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
}

$session = "h$probeId"
$env:HERDR_SESSION = $session
$env:HERDR_PROBE_EXE = $exe
$env:HERDR_PROBE_WORKING_DIRECTORY = (Get-Location).ProviderPath
$env:XDG_CONFIG_HOME = $temporary
$env:XDG_STATE_HOME = $temporary

$watcher = New-Object IO.FileSystemWatcher $bootstrapBase
$watcher.IncludeSubdirectories = $true
$watcher.NotifyFilter = [IO.NotifyFilters]::DirectoryName -bor [IO.NotifyFilters]::FileName
$watcher.EnableRaisingEvents = $true
$eventSource = "herdr-interactive-server-probe-$probeId"
$eventSubscription = Register-ObjectEvent -InputObject $watcher -EventName Created `
    -SourceIdentifier $eventSource
$serverProcessId = $null
$bootstrapObserved = $false
$completed = $false

try {
    $jobHelper = Join-Path $PSScriptRoot 'windows_interactive_server_probe_job.cs'
    if (-not (Test-Path -LiteralPath $jobHelper -PathType Leaf)) {
        throw "interactive server probe job helper is missing: $jobHelper"
    }
    if ($null -eq ('HerdrWin.Probes.KillOnCloseProcess' -as [type])) {
        Add-Type -Path $jobHelper
    }
    $argumentLine = '/d /q /c ""{0}""' -f $batch
    $launcherExitCode = [HerdrWin.Probes.KillOnCloseProcess]::Run(
        $env:ComSpec,
        $argumentLine,
        (Get-Location).ProviderPath,
        $TimeoutSeconds * 1000
    )
    $events = @(Get-Event -SourceIdentifier $eventSource -ErrorAction SilentlyContinue)
    if ($events.Count -eq 0) {
        Wait-Event -SourceIdentifier $eventSource -Timeout 1 | Out-Null
        $events = @(Get-Event -SourceIdentifier $eventSource -ErrorAction SilentlyContinue)
    }
    $bootstrapObserved = @(
        $events | Where-Object { $_.SourceEventArgs.Name -like '*bootstrap.json' }
    ).Count -ne 0
    if ($launcherExitCode -ne 0) {
        $detail = 'cmd.exe server start failed; see the command output above'
        $serverLog = Join-Path $env:XDG_CONFIG_HOME "herdr\sessions\$session\herdr-server.log"
        if ([IO.File]::Exists($serverLog)) {
            $logDetail = [IO.File]::ReadAllText($serverLog)
            if ($logDetail.Length -gt 8192) {
                $logDetail = $logDetail.Substring($logDetail.Length - 8192)
            }
            $detail = "$detail`nserver log:`n$($logDetail.Trim())"
        }
        $runningDetail = @(
            Get-ExactRuntimeProcesses $exe |
                Where-Object { $initialProcessIds -notcontains [int] $_.ProcessId } |
                ForEach-Object {
                    "pid=$($_.ProcessId), session=$($_.SessionId), command=$($_.CommandLine)"
                }
        )
        if ($runningDetail.Count -ne 0) {
            $detail = "$detail`nruntime processes:`n$($runningDetail -join [Environment]::NewLine)"
        }
        if ([IO.Directory]::Exists($env:XDG_CONFIG_HOME)) {
            $files = @(
                [IO.Directory]::EnumerateFiles($env:XDG_CONFIG_HOME, '*', [IO.SearchOption]::AllDirectories) |
                    ForEach-Object { $_.Substring($env:XDG_CONFIG_HOME.Length).TrimStart('\') }
            )
            if ($files.Count -ne 0) {
                $detail = "$detail`nconfig files:`n$($files -join [Environment]::NewLine)"
            }
        }
        throw "interactive server probe launcher failed with exit code ${launcherExitCode}: $detail"
    }
    if (-not $bootstrapObserved) {
        throw 'server start did not use the Task Scheduler interactive launch path'
    }

    $newProcesses = @(
        Get-ExactRuntimeProcesses $exe |
            Where-Object { $initialProcessIds -notcontains [int] $_.ProcessId }
    )
    if ($newProcesses.Count -ne 1) {
        throw "interactive server probe expected one new runtime process, found $($newProcesses.Count)"
    }
    $server = $newProcesses[0]
    $serverProcessId = [int] $server.ProcessId
    $currentSession = (Get-Process -Id $PID).SessionId
    if ([int] $server.SessionId -ne [int] $currentSession) {
        throw "interactive server launched in session $($server.SessionId), expected $currentSession"
    }
    $owner = Invoke-CimMethod -InputObject $server -MethodName GetOwner
    if ($owner.ReturnValue -ne 0) {
        throw "could not resolve interactive server owner, Win32 status $($owner.ReturnValue)"
    }
    $actualOwner = "$($owner.Domain)\$($owner.User)"
    $expectedOwner = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    if (-not [string]::Equals($actualOwner, $expectedOwner, [StringComparison]::OrdinalIgnoreCase)) {
        throw "interactive server launched as $actualOwner, expected $expectedOwner"
    }

    $statusOutput = @(& $exe status server --json 2>&1)
    $statusExitCode = $LASTEXITCODE
    if ($statusExitCode -ne 0) {
        throw "interactive server status failed with exit code ${statusExitCode}: $($statusOutput -join [Environment]::NewLine)"
    }
    $status = ($statusOutput -join [Environment]::NewLine) | ConvertFrom-Json
    if ($status.running -ne $true) {
        throw 'interactive server status did not report a running server'
    }
    if (@(Get-InteractiveTaskNames).Count -ne 0) {
        throw 'interactive server launch left scheduled task residue'
    }
    if (@(Get-InteractiveLaunchDirectories $bootstrapBase).Count -ne 0) {
        throw 'interactive server did not consume its bootstrap state'
    }

    $stopOutput = @(& $exe server stop 2>&1)
    $stopExitCode = $LASTEXITCODE
    if ($stopExitCode -ne 0) {
        throw "interactive server stop failed with exit code ${stopExitCode}: $($stopOutput -join [Environment]::NewLine)"
    }
    $serverProcess = Get-Process -Id $serverProcessId -ErrorAction SilentlyContinue
    if ($null -ne $serverProcess -and -not $serverProcess.WaitForExit(10000)) {
        throw 'interactive server did not exit within 10 seconds after stop'
    }
    $serverProcessId = $null

    if (@(Get-InteractiveTaskNames).Count -ne 0) {
        throw 'interactive server stop left scheduled task residue'
    }
    if (@(Get-InteractiveLaunchDirectories $bootstrapBase).Count -ne 0) {
        throw 'interactive server stop left bootstrap residue'
    }
    $completed = $true
} finally {
    if ($null -ne $serverProcessId) {
        try { & $exe server stop 2>&1 | Out-Null } catch {}
    }
    Stop-OwnedRuntimeProcesses $exe $initialProcessIds
    Unregister-Event -SourceIdentifier $eventSource -ErrorAction SilentlyContinue
    Remove-Event -SourceIdentifier $eventSource -ErrorAction SilentlyContinue
    $watcher.Dispose()
    foreach ($name in $savedEnvironment.Keys) {
        if ($null -eq $savedEnvironment[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
        }
    }
    if ([IO.Directory]::Exists($temporary)) {
        Remove-Item -LiteralPath $temporary -Recurse -Force
    }
    if ($createdBootstrapBase -and [IO.Directory]::Exists($bootstrapBase) -and
        [IO.Directory]::GetFileSystemEntries($bootstrapBase).Count -eq 0) {
        [IO.Directory]::Delete($bootstrapBase)
    }
}

if (-not $completed) {
    throw 'interactive server launch probe did not reach a terminal result'
}
$stopwatch.Stop()
"interactive_server_launch_probe=passed"
"parent=cmd.exe"
"session=$currentSession"
"owner=$expectedOwner"
"elapsed_seconds=$('{0:F3}' -f $stopwatch.Elapsed.TotalSeconds)"
