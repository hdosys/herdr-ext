[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$StageDir,
    [Parameter(Mandatory = $true)][string]$LauncherExe,
    [Parameter(Mandatory = $true)][string]$InstallerHelperExe,
    [Parameter(Mandatory = $true)][string]$BuildId,
    [string]$BuildFreshness,
    [Parameter(Mandatory = $true)][string]$ReleaseVersion,
    [Parameter(Mandatory = $true)][string]$BaseVersion,
    [Parameter(Mandatory = $true)][string]$OutputDir,
    [string]$ProductName = "Herdr",
    [string]$PackageName = "Herdr Win",
    [string]$AgentUserProfileRoot,
    [string[]]$Faults = @(
        "after-bin-directory",
        "after-state-directory",
        "after-uninstaller",
        "after-arp-registration"
    )
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($ReleaseVersion -ceq "local") {
    $freshnessMatch = [regex]::Match(
        $BuildFreshness,
        '^(?<year>[0-9]{4})\.(?<month>[0-9]{2})\.(?<day>[0-9]{2})\.(?<hour>[0-9]{2})(?<minute>[0-9]{2})Z$'
    )
    $freshness = [DateTime]::MinValue
    if (-not $freshnessMatch.Success -or -not [DateTime]::TryParseExact(
        $BuildFreshness,
        "yyyy.MM.dd.HHmm'Z'",
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::AssumeUniversal -bor [Globalization.DateTimeStyles]::AdjustToUniversal,
        [ref]$freshness
    )) {
        throw "BuildFreshness must be a real UTC YYYY.MM.DD.HHMMZ value for local builds."
    }
    $DisplayVersion = "$BuildFreshness (local, build $BuildId)"
    $NumericVersion = "$([int]$freshnessMatch.Groups['year'].Value).$([int]$freshnessMatch.Groups['month'].Value).$([int]$freshnessMatch.Groups['day'].Value).$([int]$freshnessMatch.Groups['hour'].Value * 100 + [int]$freshnessMatch.Groups['minute'].Value)"
} else {
    $releaseMatch = [regex]::Match(
        $ReleaseVersion,
        '^(?<year>[0-9]{4})\.(?<month>[0-9]{2})\.(?<day>[0-9]{2})\.(?<sequence>[1-9][0-9]*)$'
    )
    if (-not $releaseMatch.Success -or [uint64]$releaseMatch.Groups['sequence'].Value -gt 65535) {
        throw "ReleaseVersion must be 'local' or a Windows-compatible YYYY.MM.DD.N CalVer."
    }
    $DisplayVersion = $ReleaseVersion
    $NumericVersion = "$([int]$releaseMatch.Groups['year'].Value).$([int]$releaseMatch.Groups['month'].Value).$([int]$releaseMatch.Groups['day'].Value).$([int]$releaseMatch.Groups['sequence'].Value)"
}

$projectRoot = Split-Path -Parent $PSScriptRoot
$packager = Join-Path $PSScriptRoot "package_windows_installer.ps1"
$originalUserProfile = $env:USERPROFILE
$originalLocalAppData = $env:LOCALAPPDATA
$originalClaudeConfigDir = $env:CLAUDE_CONFIG_DIR
$originalXdgConfigHome = $env:XDG_CONFIG_HOME
$originalSession = $env:HERDR_SESSION
$originalSocketPath = $env:HERDR_SOCKET_PATH
$originalClientSocketPath = $env:HERDR_CLIENT_SOCKET_PATH
$originalRemoteSidecar = $env:HERDR_REMOTE_SIDECAR_V1
$userEnvironmentKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $false)
if ($null -eq $userEnvironmentKey) {
    throw "HKCU\\Environment is unavailable; refusing to run the installer fault matrix."
}
try {
    $originalUserPathExists = @($userEnvironmentKey.GetValueNames()) -contains "Path"
    $originalUserPath = if ($originalUserPathExists) {
        $userEnvironmentKey.GetValue(
            "Path",
            $null,
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
    } else {
        $null
    }
    $originalUserPathKind = if ($originalUserPathExists) {
        $userEnvironmentKey.GetValueKind("Path")
    } else {
        $null
    }
} finally {
    $userEnvironmentKey.Dispose()
}

if (-not ("HerdrTestEnvironmentBroadcast" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class HerdrTestEnvironmentBroadcast {
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr SendMessageTimeout(IntPtr window, uint message, IntPtr parameter, string value, uint flags, uint timeout, out IntPtr result);
}
'@
}

function Restore-TestUserPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $true)
    if ($null -eq $key) {
        throw "HKCU\\Environment is unavailable while restoring the installer fault test."
    }
    try {
        if ($originalUserPathExists) {
            $key.SetValue("Path", $originalUserPath, $originalUserPathKind)
        } else {
            $key.DeleteValue("Path", $false)
        }
    } finally {
        $key.Dispose()
    }

    $result = [IntPtr]::Zero
    $sent = [HerdrTestEnvironmentBroadcast]::SendMessageTimeout(
        [IntPtr]0xffff,
        0x001A,
        [IntPtr]::Zero,
        "Environment",
        0x0002,
        100,
        [ref]$result
    )
    if ($sent -eq [IntPtr]::Zero) {
        throw "Restored the user PATH but failed to broadcast the environment change."
    }
}

function Test-TestUserPathRestored {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $false)
    if ($null -eq $key) {
        return $false
    }
    try {
        $actualExists = @($key.GetValueNames()) -contains "Path"
        if ($actualExists -ne $originalUserPathExists) {
            return $false
        }
        if ($actualExists) {
            $actualValue = $key.GetValue(
                "Path",
                $null,
                [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
            )
            $actualKind = $key.GetValueKind("Path")
            if ([string]$actualValue -cne [string]$originalUserPath -or $actualKind -ne $originalUserPathKind) {
                return $false
            }
        }
        return $true
    } finally {
        $key.Dispose()
    }
}

function Assert-TestUserPathRestored {
    if (-not (Test-TestUserPathRestored)) {
        throw "Uninstall did not restore the exact user PATH value and registry kind."
    }
}

function Test-TestUserPathRetryOwned {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $false)
    if ($null -eq $key) {
        return $false
    }
    try {
        if (@($key.GetValueNames()) -notcontains "Path") {
            return $false
        }
        $actualValue = [string]$key.GetValue(
            "Path",
            $null,
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
        $actualKind = $key.GetValueKind("Path")
        $managedBin = Join-Path $installRoot "bin"
        $expectedValue = if ($originalUserPathExists -and -not [string]::IsNullOrEmpty([string]$originalUserPath)) {
            "$managedBin;$originalUserPath"
        } else {
            $managedBin
        }
        $expectedKind = if ($originalUserPathExists) {
            $originalUserPathKind
        } else {
            [Microsoft.Win32.RegistryValueKind]::ExpandString
        }
        return $actualValue -ceq $expectedValue -and $actualKind -eq $expectedKind
    } finally {
        $key.Dispose()
    }
}

function Assert-TestUserPathRetryOwned {
    if (-not (Test-TestUserPathRetryOwned)) {
        throw "Failed uninstall did not restore the exact installer-owned PATH state."
    }
}

$ownsAgentUserProfile = [string]::IsNullOrWhiteSpace($AgentUserProfileRoot)
function Remove-TestOwnedUserProfile {
    if (-not $ownsAgentUserProfile -or [string]::IsNullOrWhiteSpace($AgentUserProfileRoot)) {
        return
    }
    if (Test-Path -LiteralPath $AgentUserProfileRoot) {
        Remove-Item -LiteralPath $AgentUserProfileRoot -Recurse -Force -ErrorAction Stop
    }
    if (Test-Path -LiteralPath $AgentUserProfileRoot) {
        throw "Installer fault test left its temporary user profile behind: $AgentUserProfileRoot"
    }
}

try {
if ($ownsAgentUserProfile) {
    $AgentUserProfileRoot = Join-Path ([IO.Path]::GetTempPath()) ("hs%-" + [Guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Path $AgentUserProfileRoot | Out-Null
} elseif (-not (Test-Path -LiteralPath $AgentUserProfileRoot -PathType Container)) {
    throw "AgentUserProfileRoot must be an existing test-owned directory: $AgentUserProfileRoot"
}
$AgentUserProfileRoot = [IO.Path]::GetFullPath($AgentUserProfileRoot)
$env:USERPROFILE = $AgentUserProfileRoot
$env:LOCALAPPDATA = Join-Path $env:USERPROFILE "AppData\Local"
$env:CLAUDE_CONFIG_DIR = Join-Path $env:USERPROFILE ".claude"
$env:XDG_CONFIG_HOME = Join-Path $AgentUserProfileRoot "xdg-config"
Remove-Item Env:HERDR_SESSION, Env:HERDR_SOCKET_PATH, Env:HERDR_CLIENT_SOCKET_PATH, Env:HERDR_REMOTE_SIDECAR_V1 -ErrorAction SilentlyContinue
if (-not (Test-Path -LiteralPath $env:LOCALAPPDATA)) {
    New-Item -ItemType Directory -Path $env:LOCALAPPDATA -Force | Out-Null
}
if (-not (Test-Path -LiteralPath $env:XDG_CONFIG_HOME)) {
    New-Item -ItemType Directory -Path $env:XDG_CONFIG_HOME -Force | Out-Null
}
$installRoot = Join-Path $env:LOCALAPPDATA "Programs\$ProductName"
$arpKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$PackageName"
$skillSource = Join-Path $projectRoot "skills\herdr\SKILL.md"
$skillRoot = Join-Path $env:USERPROFILE ".agents\skills\herdr"
$skillPath = Join-Path $skillRoot "SKILL.md"
$claudeSkillRoot = Join-Path $env:CLAUDE_CONFIG_DIR "skills\herdr"
$claudeSkillPath = Join-Path $claudeSkillRoot "SKILL.md"
$settingsRoot = Join-Path $env:USERPROFILE ".herdr"
$inheritedUserProfileDecoy = Join-Path $AgentUserProfileRoot "inherited-userprofile-decoy"
New-Item -ItemType Directory -Path $inheritedUserProfileDecoy | Out-Null
$env:USERPROFILE = $inheritedUserProfileDecoy
$allowedFaults = @(
    "after-bin-directory",
    "after-state-directory",
    "after-uninstaller",
    "after-arp-registration"
)
$hardTerminationFault = "terminate-after-installer-helper"
$cleanupFaults = @($allowedFaults) + @($hardTerminationFault)
if ($ProductName -cnotmatch '^[A-Za-z0-9](?:[A-Za-z0-9 ._-]{0,62}[A-Za-z0-9_-])?$') {
    throw "Invalid product name '$ProductName'."
}
function Wait-TestCondition {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$Condition,
        [Parameter(Mandatory = $true)][string]$Description,
        [int]$TimeoutMilliseconds = 30000
    )

    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.ElapsedMilliseconds -lt $TimeoutMilliseconds) {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 100
    }
    throw "$Description did not reach terminal state within $TimeoutMilliseconds ms."
}

function Start-TestProcess {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$Arguments = @(),
        [int]$TimeoutMilliseconds = 120000
    )

    $process = Start-Process -FilePath $FilePath -ArgumentList $Arguments -PassThru
    try {
        if (-not $process.WaitForExit($TimeoutMilliseconds)) {
            $taskkill = Join-Path $env:SystemRoot "System32\taskkill.exe"
            $cleanup = Start-Process -FilePath $taskkill -ArgumentList @(
                "/PID", $process.Id, "/T", "/F"
            ) -PassThru -NoNewWindow
            try {
                if (-not $cleanup.WaitForExit(10000)) {
                    $cleanup.Kill()
                    [void]$cleanup.WaitForExit(5000)
                }
            } finally {
                $cleanup.Dispose()
            }
            if (-not $process.WaitForExit(10000)) {
                $process.Kill()
                [void]$process.WaitForExit(5000)
            }
            throw "$FilePath exceeded its $TimeoutMilliseconds ms process deadline."
        }
        return $process.ExitCode
    } finally {
        $process.Dispose()
    }
}

function New-TestIdentityLauncher {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Identity
    )

    $source = "$Path.cs"
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    [IO.File]::WriteAllText($source, @"
using System;
internal static class Program {
    public static int Main(string[] args) {
        if (args.Length == 1 && String.Equals(args[0], "--herdr-private-launcher-build-id-v1", StringComparison.Ordinal)) {
            Console.Out.WriteLine("$Identity");
            return 0;
        }
        return 64;
    }
}
"@, [Text.UTF8Encoding]::new($false))
    $compiler = @(
        "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe",
        "$env:WINDIR\Microsoft.NET\Framework\v4.0.30319\csc.exe"
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if ($null -eq $compiler) {
        throw "The Windows .NET Framework C# compiler is required for the pending-update test."
    }
    $exitCode = Start-TestProcess -FilePath $compiler -Arguments @(
        "/nologo", "/target:exe", "/platform:x64", "/out:$Path", $source
    )
    if ($exitCode -ne 0 -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Could not build the pending-update launcher fixture."
    }
}

function Copy-TestCanonicalText {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination
    )

    $text = (New-Object Text.UTF8Encoding($false, $true)).GetString([IO.File]::ReadAllBytes($Source))
    $normalized = $text.Replace("`r`n", "`n")
    if ($normalized.Contains("`r") -or
        -not $normalized.EndsWith("`n", [StringComparison]::Ordinal)) {
        throw "Canonical test text must use valid line endings and end with a newline: $Source"
    }

    # Exercise the Windows checkout variant locally before writing package bytes.
    $checkoutVariant = $normalized.Replace("`n", "`r`n")
    $canonical = $checkoutVariant.Replace("`r`n", "`n")
    [IO.File]::WriteAllBytes(
        $Destination,
        (New-Object Text.UTF8Encoding($false)).GetBytes($canonical)
    )
}

function New-TestHelperPackage {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$AppLauncher,
        [Parameter(Mandatory = $true)][string]$Uninstaller
    )

    if (Test-Path -LiteralPath $Root) {
        throw "Native helper package fixture already exists: $Root"
    }
    New-Item -ItemType Directory -Path $Root | Out-Null
    Copy-Item -LiteralPath $StageDir -Destination (Join-Path $Root "payload") -Recurse
    New-Item -ItemType Directory -Path (Join-Path $Root "skill") | Out-Null
    [IO.File]::Copy($AppLauncher, (Join-Path $Root "app-launcher.exe"), $false)
    [IO.File]::Copy($InstallerHelperExe, (Join-Path $Root "installer-helper.exe"), $false)
    Copy-TestCanonicalText -Source $skillSource -Destination (Join-Path $Root "skill\SKILL.md")
    $skillHashManifest = Join-Path $projectRoot "packaging\windows\managed-skill-hashes.txt"
    Copy-TestCanonicalText -Source $skillHashManifest -Destination (Join-Path $Root "skill\managed-skill-hashes.txt")
    [IO.File]::Copy($Uninstaller, (Join-Path $Root "uninstall.exe"), $false)
}

function Start-TestLeaseHolder {
    param(
        [Parameter(Mandatory = $true)][string]$LeasePath,
        [Parameter(Mandatory = $true)][string]$ReadyPath
    )

    $escapedLease = $LeasePath.Replace("'", "''")
    $escapedReady = $ReadyPath.Replace("'", "''")
    $releaseName = "Local\HerdrWinTestLeaseRelease-$([Guid]::NewGuid().ToString('N'))"
    $escapedReleaseName = $releaseName.Replace("'", "''")
    $release = [Threading.EventWaitHandle]::new(
        $false,
        [Threading.EventResetMode]::ManualReset,
        $releaseName
    )
    $command = @"
`$ErrorActionPreference = 'Stop'
`$release = [Threading.EventWaitHandle]::OpenExisting('$escapedReleaseName')
`$lease = [IO.File]::Open('$escapedLease', [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite)
[IO.File]::WriteAllText('$escapedReady', 'ready')
try {
    if (-not `$release.WaitOne(300000)) {
        throw 'Timed out waiting to release the pending-update test lease.'
    }
} finally {
    `$lease.Dispose()
    `$release.Dispose()
}
"@
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    try {
        $process = Start-Process -FilePath powershell.exe -ArgumentList @(
            "-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", $encoded
        ) -PassThru -WindowStyle Hidden
        return [PSCustomObject]@{
            Process = $process
            Release = $release
        }
    } catch {
        $release.Dispose()
        throw
    }
}

function Get-TestFileSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [IO.File]::Open(
        $Path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    )
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace("-", "")
    } finally {
        $hasher.Dispose()
        $stream.Dispose()
    }
}

function Invoke-TestQuietUninstall {
    $helper = Join-Path $installRoot "state\installer-helper.exe"
    if (-not (Test-Path -LiteralPath $helper -PathType Leaf)) {
        throw "Native quiet-uninstall helper is missing: $helper"
    }
    $expected = ('"{0}" quiet-uninstall --install-root "{1}"' -f $helper, $installRoot)
    $actual = [string](Get-ItemProperty -LiteralPath $arpKey).QuietUninstallString
    if ($actual -cne $expected) {
        throw "ARP quiet uninstall command is not exact. Expected '$expected', got '$actual'."
    }
    return Start-TestProcess -FilePath $helper -Arguments @(
        "quiet-uninstall",
        "--install-root", ('"' + $installRoot + '"')
    )
}

function Wait-TestInstallerIdle {
    param([int]$TimeoutMilliseconds = 30000)

    Wait-TestCondition -TimeoutMilliseconds $TimeoutMilliseconds -Description "installer lifecycle" -Condition {
        $mutex = New-Object System.Threading.Mutex($false, "Local\HerdrWinInstallerLifecycle")
        try {
            try {
                $acquired = $mutex.WaitOne(0)
            } catch [System.Threading.AbandonedMutexException] {
                $acquired = $true
            }
            if (-not $acquired) {
                return $false
            }
            $mutex.ReleaseMutex()
            return $true
        } finally {
            $mutex.Dispose()
        }
    }
}

function Remove-TestInstallIfPresent {
    if (-not (Test-Path -LiteralPath $installRoot)) {
        return
    }
    $uninstaller = Join-Path $installRoot "uninstall.exe"
    if (-not (Test-Path -LiteralPath $uninstaller -PathType Leaf)) {
        throw "Cannot safely clean unexpected test install root: $installRoot"
    }
    foreach ($fault in $cleanupFaults) {
        [IO.File]::WriteAllText(
            (Join-Path $env:TEMP "herdr-uninstall-fault-$fault.once"),
            "cleanup"
        )
    }
    [void](Start-TestProcess -FilePath $uninstaller -Arguments @("/S"))
    Wait-TestCondition -Description "test install cleanup" -Condition {
        -not (Test-Path -LiteralPath $installRoot)
    }
}

function Assert-TestSkillInstalled {
    $skillText = (New-Object Text.UTF8Encoding($false, $true)).GetString([IO.File]::ReadAllBytes($skillSource))
    $skillValidationText = $skillText.Replace("`r`n", "`n")
    if ($skillValidationText.Contains("`r") -or
        -not $skillValidationText.EndsWith("`n", [StringComparison]::Ordinal)) {
        throw "Managed skill source must use valid line endings and end with a newline."
    }
    $expectedBytes = (New-Object Text.UTF8Encoding($false)).GetBytes($skillValidationText)
    $expected = [Convert]::ToBase64String($expectedBytes)
    foreach ($candidate in @($skillPath, $claudeSkillPath)) {
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            throw "Managed installer did not publish SKILL.md: $candidate"
        }
        $actual = [Convert]::ToBase64String([IO.File]::ReadAllBytes($candidate))
        if ($actual -cne $expected) {
            throw "Managed installer did not publish the canonical SKILL.md: $candidate"
        }
    }
    foreach ($sibling in @(
        (Join-Path $skillRoot "previous-resources\old.txt"),
        (Join-Path $claudeSkillRoot "previous-resources\old.txt")
    )) {
        if ([IO.File]::ReadAllText($sibling) -cne "previous resource") {
            throw "Managed installer removed or changed a foreign skill sibling: $sibling"
        }
    }
}

foreach ($fault in $Faults) {
    if ($allowedFaults -cnotcontains $fault) {
        throw "Unsupported uninstall fault point: $fault"
    }
}
if (-not (Test-Path -LiteralPath $projectRoot -PathType Container)) {
    throw "Project root is missing: $projectRoot"
}
if (Test-Path -LiteralPath $installRoot) {
    throw "Fault test requires no existing Herdr install: $installRoot"
}
if (Test-Path -LiteralPath $arpKey) {
    throw "Fault test requires no existing Herdr ARP registration."
}
if (Test-Path -LiteralPath $skillRoot) {
    throw "Fault test requires no existing cross-agent Herdr skill: $skillRoot"
}
if (Test-Path -LiteralPath $claudeSkillRoot) {
    throw "Fault test requires no existing Claude Herdr skill: $claudeSkillRoot"
}
if (-not (Test-Path -LiteralPath $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}

$testFailure = $null
try {
    foreach ($fault in $Faults) {
        $faultMarker = Join-Path $env:TEMP "herdr-uninstall-fault-$fault.once"
        $installFailure = Join-Path $env:TEMP "herdr-install-failure-$fault.txt"
        if (Test-Path -LiteralPath $faultMarker) {
            Remove-Item -LiteralPath $faultMarker -Force
        }
        if (Test-Path -LiteralPath $installFailure) {
            Remove-Item -LiteralPath $installFailure -Force
        }
        $installer = Join-Path $OutputDir "herdr-installer-fault-$fault.exe"
        if (Test-Path -LiteralPath $installer) {
            Remove-Item -LiteralPath $installer -Force
        }
        New-Item -ItemType Directory -Path (Join-Path $skillRoot "previous-resources") -Force | Out-Null
        Copy-TestCanonicalText -Source $skillSource -Destination $skillPath
        [IO.File]::WriteAllText((Join-Path $skillRoot "previous-resources\old.txt"), "previous resource")
        New-Item -ItemType Directory -Path (Join-Path $claudeSkillRoot "previous-resources") -Force | Out-Null
        Copy-TestCanonicalText -Source $skillSource -Destination $claudeSkillPath
        [IO.File]::WriteAllText((Join-Path $claudeSkillRoot "previous-resources\old.txt"), "previous resource")

        & $packager `
            -StageDir $StageDir `
            -LauncherExe $LauncherExe `
            -InstallerHelperExe $InstallerHelperExe `
            -BuildId $BuildId `
            -BuildFreshness $BuildFreshness `
            -ReleaseVersion $ReleaseVersion `
            -BaseVersion $BaseVersion `
            -ProductName $ProductName `
            -OutputPath $installer `
            -TestUninstallFault $fault `
            -TestUserProfileRoot $AgentUserProfileRoot

        $installExitCode = Start-TestProcess -FilePath $installer -Arguments @("/S")
        if ($installExitCode -ne 0) {
            $detail = if (Test-Path -LiteralPath $installFailure -PathType Leaf) {
                [IO.File]::ReadAllText($installFailure)
            } else {
                "no installer diagnostic was produced"
            }
            throw "Fresh installer for $fault exited with $installExitCode`: $detail"
        }
        try {
            Wait-TestCondition -Description "fresh install for $fault" -Condition {
                (Test-Path -LiteralPath (Join-Path $installRoot "state\active")) -and
                    (Test-Path -LiteralPath $arpKey)
            }
        } catch {
            throw "Fresh install for $fault did not publish expected state at $installRoot (root=$(Test-Path -LiteralPath $installRoot), active=$(Test-Path -LiteralPath (Join-Path $installRoot 'state\active')), arp=$(Test-Path -LiteralPath $arpKey), exit=$installExitCode)."
        }
        Assert-TestSkillInstalled
        New-Item -ItemType Directory -Path $settingsRoot -Force | Out-Null
        [IO.File]::WriteAllText((Join-Path $settingsRoot "settings.toml"), "preserve-by-default")

        $uninstaller = Join-Path $installRoot "uninstall.exe"
        if ($fault -eq "after-bin-directory") {
            $firstDirectExit = Start-TestProcess -FilePath $uninstaller -Arguments @("/S")
            if ($firstDirectExit -ne 0) {
                throw "Direct uninstall bootstrap for $fault exited with $firstDirectExit."
            }
        } else {
            $firstQuietExit = Invoke-TestQuietUninstall
            if ($firstQuietExit -eq 0) {
                throw "Quiet uninstall reported success after injected failure $fault."
            }
        }
        Wait-TestCondition -Description "first injected uninstall for $fault" -Condition {
            Test-Path -LiteralPath $faultMarker
        }
        Wait-TestCondition -Description "restored uninstall retry ownership for $fault" -Condition {
            (Test-Path -LiteralPath $uninstaller -PathType Leaf) -and
                (Test-Path -LiteralPath (Join-Path $installRoot "state\installer-helper.exe") -PathType Leaf) -and
                (Test-Path -LiteralPath (Join-Path $installRoot "state\uninstall.pending") -PathType Leaf) -and
                (Test-Path -LiteralPath $arpKey) -and
                (Test-TestUserPathRetryOwned)
        }
        Wait-TestInstallerIdle
        if (Test-Path -LiteralPath $skillPath) {
            throw "Injected uninstall $fault retained universal SKILL.md."
        }
        if (Test-Path -LiteralPath $claudeSkillPath) {
            throw "Injected uninstall $fault retained Claude SKILL.md."
        }
        if ([IO.File]::ReadAllText((Join-Path $skillRoot "previous-resources\old.txt")) -cne "previous resource" -or
            [IO.File]::ReadAllText((Join-Path $claudeSkillRoot "previous-resources\old.txt")) -cne "previous resource") {
            throw "Injected uninstall $fault removed a foreign skill sibling."
        }
        if ([IO.File]::ReadAllText((Join-Path $settingsRoot "settings.toml")) -cne "preserve-by-default") {
            throw "Injected uninstall $fault did not preserve settings by default."
        }
        if (-not (Test-Path -LiteralPath $uninstaller -PathType Leaf)) {
            throw "Injected uninstall $fault removed its retry executable."
        }
        if (-not (Test-Path -LiteralPath (Join-Path $installRoot "state\installer-helper.exe") -PathType Leaf)) {
            throw "Injected uninstall $fault removed its native quiet retry helper."
        }
        if (-not (Test-Path -LiteralPath (Join-Path $installRoot "state\uninstall.pending") -PathType Leaf)) {
            throw "Injected uninstall $fault removed its final ownership sentinel."
        }
        Assert-TestUserPathRetryOwned

        if ($fault -eq "after-state-directory") {
            $setupRetryExit = Start-TestProcess -FilePath $installer -Arguments @("/S")
            if ($setupRetryExit -ne 0) {
                throw "Setup retry $fault exited with $setupRetryExit."
            }
            Wait-TestCondition -Description "setup retry for $fault" -Condition {
                (Test-Path -LiteralPath (Join-Path $installRoot "state\active") -PathType Leaf) -and
                    (Test-Path -LiteralPath $arpKey)
            }
            $retryQuietExit = Invoke-TestQuietUninstall
            if ($retryQuietExit -ne 0) {
                throw "Quiet uninstall after setup retry $fault exited with $retryQuietExit."
            }
        } elseif ($fault -eq "after-uninstaller") {
            $retryDirectExit = Start-TestProcess -FilePath $uninstaller -Arguments @("/S")
            if ($retryDirectExit -ne 0) {
                throw "Direct uninstall retry $fault exited with $retryDirectExit."
            }
        } else {
            $retryQuietExit = Invoke-TestQuietUninstall
            if ($retryQuietExit -ne 0) {
                throw "Quiet uninstall retry $fault exited with $retryQuietExit."
            }
        }
        Wait-TestCondition -Description "retry uninstall for $fault" -Condition {
            -not (Test-Path -LiteralPath $installRoot) -and
                -not (Test-Path -LiteralPath $arpKey) -and
                -not (Test-Path -LiteralPath $faultMarker)
        }
        Assert-TestUserPathRestored
        if ([IO.File]::ReadAllText((Join-Path $settingsRoot "settings.toml")) -cne "preserve-by-default") {
            throw "Retry uninstall $fault did not preserve settings by default."
        }
        Remove-Item -LiteralPath $settingsRoot -Recurse -Force
        if ($fault -eq "after-bin-directory" -or $fault -eq "after-uninstaller") {
            Write-Host "Cross-mode uninstall retry passed: $fault"
        }
        if ($fault -eq "after-state-directory") {
            Write-Host "Setup retry ownership passed: $fault"
        }
        Write-Host "Uninstall fault retry passed: $fault"
    }

    $savedOriginalUserPathExists = $originalUserPathExists
    $savedOriginalUserPath = $originalUserPath
    $savedOriginalUserPathKind = $originalUserPathKind
    $originalUserPathExists = $false
    $originalUserPath = $null
    $originalUserPathKind = $null
    try {
    Restore-TestUserPath
    $installFaults = @(
        [pscustomobject]@{ Name = "after-user-path"; PartialArp = $false },
        [pscustomobject]@{ Name = "after-arp-path-added"; PartialArp = $true }
    )
    foreach ($installFaultCase in $installFaults) {
        $installFault = $installFaultCase.Name
        $installFaultMarker = Join-Path $env:TEMP "herdr-uninstall-fault-install-$installFault.once"
        $installFaultInstaller = Join-Path $OutputDir "herdr-installer-fault-install-$installFault.exe"
        foreach ($path in @($installFaultMarker, $installFaultInstaller)) {
            if (Test-Path -LiteralPath $path) {
                Remove-Item -LiteralPath $path -Force
            }
        }
        & $packager `
            -StageDir $StageDir `
            -LauncherExe $LauncherExe `
            -InstallerHelperExe $InstallerHelperExe `
            -BuildId $BuildId `
            -BuildFreshness $BuildFreshness `
            -ReleaseVersion $ReleaseVersion `
            -BaseVersion $BaseVersion `
            -ProductName $ProductName `
            -OutputPath $installFaultInstaller `
            -TestInstallFault $installFault `
            -TestUserProfileRoot $AgentUserProfileRoot
        $firstInstallFaultExit = Start-TestProcess -FilePath $installFaultInstaller -Arguments @("/S")
        if ($firstInstallFaultExit -eq 0) {
            throw "Setup reported success after injected ownership failure $installFault."
        }
        Wait-TestCondition -Description "interrupted ownership publication at $installFault" -Condition {
            $arpReady = if ($installFaultCase.PartialArp) {
                if (-not (Test-Path -LiteralPath $arpKey)) {
                    $false
                } else {
                    $partialArp = Get-ItemProperty -LiteralPath $arpKey
                    $partialNames = @((Get-Item -LiteralPath $arpKey).Property)
                    [int]$partialArp.PathAdded -eq 1 -and $partialNames -notcontains "PathValueCreated"
                }
            } else {
                -not (Test-Path -LiteralPath $arpKey)
            }
            (Test-Path -LiteralPath (Join-Path $installRoot "state\path-add.pending") -PathType Leaf) -and
                (Test-Path -LiteralPath $installFaultMarker -PathType Leaf) -and
                $arpReady -and
                (Test-TestUserPathRetryOwned)
        }
        $retryInstallFaultExit = Start-TestProcess -FilePath $installFaultInstaller -Arguments @("/S")
        if ($retryInstallFaultExit -ne 0) {
            throw "Setup ownership retry $installFault exited with $retryInstallFaultExit."
        }
        Wait-TestCondition -Description "recovered ownership publication at $installFault" -Condition {
            (Test-Path -LiteralPath (Join-Path $installRoot "state\active") -PathType Leaf) -and
                -not (Test-Path -LiteralPath (Join-Path $installRoot "state\path-add.pending")) -and
                (Test-Path -LiteralPath $arpKey)
        }
        $recoveredArp = Get-ItemProperty -LiteralPath $arpKey
        if ([int]$recoveredArp.PathAdded -ne 1) {
            throw "Recovered setup did not retain ownership of its pending PATH entry."
        }
        if ([int]$recoveredArp.PathValueCreated -ne 1) {
            throw "Recovered setup did not retain ownership of its newly created PATH value."
        }
        $installFaultUninstallExit = Invoke-TestQuietUninstall
        if ($installFaultUninstallExit -ne 0) {
            throw "Uninstall after ownership recovery $installFault exited with $installFaultUninstallExit."
        }
        Wait-TestCondition -Description "ownership recovery uninstall at $installFault" -Condition {
            -not (Test-Path -LiteralPath $installRoot) -and
                -not (Test-Path -LiteralPath $arpKey)
        }
        Assert-TestUserPathRestored
        Remove-Item -LiteralPath $installFaultMarker -Force
        if ($installFaultCase.PartialArp) {
            Write-Host "Interrupted ARP ownership publication recovery passed."
        } else {
            Write-Host "Interrupted PATH ownership recovery passed."
        }
    }
    } finally {
        $originalUserPathExists = $savedOriginalUserPathExists
        $originalUserPath = $savedOriginalUserPath
        $originalUserPathKind = $savedOriginalUserPathKind
        Restore-TestUserPath
    }

    $hardTerminationMarker = Join-Path $env:TEMP "herdr-uninstall-fault-$hardTerminationFault.once"
    $hardTerminationInstaller = Join-Path $OutputDir "herdr-installer-fault-$hardTerminationFault.exe"
    foreach ($path in @($hardTerminationMarker, $hardTerminationInstaller)) {
        if (Test-Path -LiteralPath $path) {
            Remove-Item -LiteralPath $path -Force
        }
    }
    & $packager `
        -StageDir $StageDir `
        -LauncherExe $LauncherExe `
        -InstallerHelperExe $InstallerHelperExe `
        -BuildId $BuildId `
        -BuildFreshness $BuildFreshness `
        -ReleaseVersion $ReleaseVersion `
        -BaseVersion $BaseVersion `
        -ProductName $ProductName `
        -OutputPath $hardTerminationInstaller `
        -TestUninstallFault $hardTerminationFault `
        -TestUserProfileRoot $AgentUserProfileRoot
    $hardInstallExit = Start-TestProcess -FilePath $hardTerminationInstaller -Arguments @("/S")
    if ($hardInstallExit -ne 0) {
        throw "Hard-termination fixture setup exited with $hardInstallExit."
    }
    $hardUninstaller = Join-Path $installRoot "uninstall.exe"
    [void](Start-TestProcess -FilePath $hardUninstaller -Arguments @("/S"))
    Wait-TestCondition -Description "hard-termination cleanup fault" -Condition {
        Test-Path -LiteralPath $hardTerminationMarker -PathType Leaf
    }
    Wait-TestInstallerIdle
    if (-not (Test-Path -LiteralPath $hardUninstaller -PathType Leaf) -or
        (Test-Path -LiteralPath (Join-Path $installRoot "state\uninstall.pending")) -or
        (Test-Path -LiteralPath (Join-Path $installRoot "state\installer-helper.exe")) -or
        (Test-Path -LiteralPath (Join-Path $installRoot "state\launcher.lock")) -or
        (Test-Path -LiteralPath $arpKey)) {
        throw "Hard-terminated cleanup did not leave the exact classifiable residual."
    }
    Assert-TestUserPathRestored
    $hardRecoveryExit = Start-TestProcess -FilePath $hardTerminationInstaller -Arguments @("/S")
    if ($hardRecoveryExit -ne 0) {
        throw "Setup recovery from hard-terminated cleanup exited with $hardRecoveryExit."
    }
    Wait-TestCondition -Description "hard-termination setup recovery" -Condition {
        (Test-Path -LiteralPath (Join-Path $installRoot "state\active") -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $installRoot "state\installer-helper.exe") -PathType Leaf) -and
            (Test-Path -LiteralPath $arpKey)
    }
    $hardCleanupExit = Invoke-TestQuietUninstall
    if ($hardCleanupExit -ne 0) {
        throw "Quiet uninstall after hard-termination recovery exited with $hardCleanupExit."
    }
    Wait-TestCondition -Description "hard-termination recovery uninstall" -Condition {
        -not (Test-Path -LiteralPath $installRoot) -and
            -not (Test-Path -LiteralPath $arpKey) -and
            -not (Test-Path -LiteralPath $hardTerminationMarker)
    }
    Assert-TestUserPathRestored
    Write-Host "Hard-termination cleanup recovery passed."

    $modifiedInstaller = Join-Path $OutputDir "herdr-installer-modified-skill-tree.exe"
    if (Test-Path -LiteralPath $modifiedInstaller) {
        Remove-Item -LiteralPath $modifiedInstaller -Force
    }
    New-Item -ItemType Directory -Path (Join-Path $skillRoot "previous-resources") -Force | Out-Null
    Copy-TestCanonicalText -Source $skillSource -Destination $skillPath
    [IO.File]::WriteAllText((Join-Path $skillRoot "previous-resources\old.txt"), "previous resource")
    New-Item -ItemType Directory -Path (Join-Path $claudeSkillRoot "previous-resources") -Force | Out-Null
    Copy-TestCanonicalText -Source $skillSource -Destination $claudeSkillPath
    [IO.File]::WriteAllText((Join-Path $claudeSkillRoot "previous-resources\old.txt"), "previous resource")
    & $packager `
        -StageDir $StageDir `
        -LauncherExe $LauncherExe `
        -InstallerHelperExe $InstallerHelperExe `
        -BuildId $BuildId `
        -BuildFreshness $BuildFreshness `
        -ReleaseVersion $ReleaseVersion `
        -BaseVersion $BaseVersion `
        -ProductName $ProductName `
        -OutputPath $modifiedInstaller `
        -TestUserProfileRoot $AgentUserProfileRoot
    $invalidSetupExit = Start-TestProcess -FilePath $modifiedInstaller -Arguments @("/S", "/WINGETjunk")
    if ($invalidSetupExit -ne 30 -or (Test-Path -LiteralPath $installRoot) -or (Test-Path -LiteralPath $arpKey)) {
        throw "Setup did not reject an unknown option before mutation with exit code 30; got $invalidSetupExit."
    }
    $modifiedInstallExit = Start-TestProcess -FilePath $modifiedInstaller -Arguments @("/S")
    if ($modifiedInstallExit -ne 0) {
        throw "Modified-tree installer exited with $modifiedInstallExit."
    }
    Wait-TestCondition -Description "modified-tree install" -Condition {
        (Test-Path -LiteralPath (Join-Path $installRoot "state\active")) -and
            (Test-Path -LiteralPath $arpKey)
    }
    Assert-TestSkillInstalled
    Write-Host "Exact setup argument rejection passed."
    Remove-ItemProperty -LiteralPath $arpKey -Name PathValueCreated
    $incompleteArpUpdateExit = Start-TestProcess -FilePath $modifiedInstaller -Arguments @("/S")
    if ($incompleteArpUpdateExit -ne 0) {
        throw "Update from the preceding current ARP registration exited with $incompleteArpUpdateExit."
    }
    $repairedArp = Get-ItemProperty -LiteralPath $arpKey
    if ([int]$repairedArp.PathAdded -ne 1 -or [int]$repairedArp.PathValueCreated -ne 0) {
        throw "Update did not repair the current ARP registration without losing PATH ownership."
    }
    Write-Host "Incomplete current ARP update repair passed."
    $wingetInstallExit = Start-TestProcess -FilePath $modifiedInstaller -Arguments @("/S", "/WINGET")
    if ($wingetInstallExit -ne 0) {
        throw "Exact /WINGET setup exited with $wingetInstallExit."
    }
    $packageManagerMarker = [IO.File]::ReadAllText((Join-Path $installRoot "state\package-manager")).Replace("`r`n", "`n")
    if ($packageManagerMarker -cne "herdr-package-manager-v1`nmanager=winget`n") {
        throw "Setup did not accept exact /WINGET package-manager ownership."
    }
    [IO.File]::WriteAllText($skillPath, "customized universal skill")
    [IO.File]::WriteAllText($claudeSkillPath, "customized Claude skill")
    New-Item -ItemType Directory -Path $settingsRoot -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $settingsRoot "settings.toml"), "remove-explicitly")
    [IO.File]::WriteAllText((Join-Path $skillRoot "user.txt"), "preserve-file")
    New-Item -ItemType Directory -Path (Join-Path $skillRoot "resources") | Out-Null
    [IO.File]::WriteAllText((Join-Path $skillRoot "resources\nested.txt"), "preserve-nested")
    $modifiedUninstaller = Join-Path $installRoot "uninstall.exe"
    $managedLauncher = Join-Path $installRoot "bin\herdr.exe"
    $managedServer = Start-Process -FilePath $managedLauncher -ArgumentList @("server") -PassThru -WindowStyle Hidden
    try {
        Wait-TestCondition -Description "managed server before uninstall" -Condition {
            try {
                $sessions = (& $managedLauncher session list --json 2>$null | Out-String | ConvertFrom-Json).sessions
                return @($sessions | Where-Object { $_.default -and $_.running }).Count -eq 1
            } catch {
                return $false
            }
        }
        # The normal NSIS bootstrap exits before its copied worker, so run the
        # exact installed image in documented direct mode to observe the
        # worker's invalid-argument exit code. The _?= root must be last.
        $prefixUninstallExit = Start-TestProcess -FilePath $modifiedUninstaller -Arguments @(
            "/S", "/REMOVE_SETTINGSjunk", "/REMOVE_SKILLjunk", "_?=$installRoot"
        )
        if ($prefixUninstallExit -ne 30) {
            throw "Unknown-option uninstall returned $prefixUninstallExit instead of 30."
        }
        Wait-TestInstallerIdle
        if ((Test-Path -LiteralPath $installRoot) -eq $false -or
            (Test-Path -LiteralPath $arpKey) -eq $false -or
            (Test-Path -LiteralPath $settingsRoot) -eq $false -or
            (Test-Path -LiteralPath $skillPath) -eq $false -or
            (Test-Path -LiteralPath $claudeSkillPath) -eq $false -or
            $managedServer.HasExited) {
            throw "Unknown uninstall options changed installed state or stopped the managed server."
        }
        $ordinaryUninstallExit = Start-TestProcess -FilePath $modifiedUninstaller -Arguments @("/S")
        if ($ordinaryUninstallExit -ne 0) {
            throw "Ordinary uninstall with a running managed server exited with $ordinaryUninstallExit."
        }
        Wait-TestCondition -Description "running-server uninstall" -Condition {
            -not (Test-Path -LiteralPath $installRoot) -and
                -not (Test-Path -LiteralPath $arpKey) -and
                $managedServer.HasExited
        }
        Wait-TestInstallerIdle
        if (-not (Test-Path -LiteralPath $settingsRoot) -or -not (Test-Path -LiteralPath $skillPath) -or -not (Test-Path -LiteralPath $claudeSkillPath)) {
            throw "Ordinary uninstall did not preserve customized user data."
        }
        Write-Host "Exact uninstall argument rejection and running-server shutdown passed."
    } finally {
        if (-not $managedServer.HasExited) {
            & taskkill.exe /PID $managedServer.Id /T /F 2>&1 | Out-Null
            [void]$managedServer.WaitForExit(5000)
        }
        $managedServer.Dispose()
    }
    $reinstallExit = Start-TestProcess -FilePath $modifiedInstaller -Arguments @("/S")
    if ($reinstallExit -ne 0) {
        throw "Reinstall after prefix-option test exited with $reinstallExit."
    }
    Wait-TestCondition -Description "reinstall after prefix-option test" -Condition {
        (Test-Path -LiteralPath (Join-Path $installRoot "state\active")) -and (Test-Path -LiteralPath $arpKey)
    }
    if ([IO.File]::ReadAllText($skillPath) -cne "customized universal skill" -or
        [IO.File]::ReadAllText($claudeSkillPath) -cne "customized Claude skill") {
        throw "Reinstall after prefix-option test overwrote customized skill content."
    }
    $modifiedUninstaller = Join-Path $installRoot "uninstall.exe"
    $modifiedUninstallExit = Start-TestProcess -FilePath $modifiedUninstaller -Arguments @("/S", "/REMOVE_SETTINGS", "/REMOVE_SKILL")
    if ($modifiedUninstallExit -ne 0) {
        throw "Modified-tree uninstaller exited with $modifiedUninstallExit."
    }
    Wait-TestCondition -Description "modified-tree uninstall" -Condition {
        -not (Test-Path -LiteralPath $installRoot) -and
            -not (Test-Path -LiteralPath $arpKey) -and
            -not (Test-Path -LiteralPath $settingsRoot)
    }
    Wait-TestInstallerIdle
    if (Test-Path -LiteralPath $skillPath) {
        throw "Sibling-preserving uninstall retained universal SKILL.md."
    }
    if (Test-Path -LiteralPath $claudeSkillPath) {
        throw "Sibling-preserving uninstall retained Claude SKILL.md."
    }
    if ([IO.File]::ReadAllText((Join-Path $skillRoot "user.txt")) -cne "preserve-file") {
        throw "Modified skill tree uninstall removed a user file."
    }
    if ([IO.File]::ReadAllText((Join-Path $skillRoot "resources\nested.txt")) -cne "preserve-nested") {
        throw "Modified skill tree uninstall removed nested user content."
    }
    if ([IO.File]::ReadAllText((Join-Path $claudeSkillRoot "previous-resources\old.txt")) -cne "previous resource") {
        throw "Sibling-preserving uninstall removed Claude skill content."
    }
    if (Test-Path -LiteralPath $settingsRoot) {
        throw "Uninstall ignored /REMOVE_SETTINGS."
    }
    Write-Host "Sibling-preserving skill uninstall passed."

    # Explicit settings cleanup is best effort after application/integration
    # removal. A real running image under .herdr keeps only that residual while
    # setup files, ARP registration, and the installer-owned PATH entry disappear.
    $lockedStateInstallExit = Start-TestProcess -FilePath $modifiedInstaller -Arguments @("/S")
    if ($lockedStateInstallExit -ne 0) {
        throw "Locked-state test install exited with $lockedStateInstallExit."
    }
    Wait-TestCondition -Description "locked-state test install" -Condition {
        (Test-Path -LiteralPath (Join-Path $installRoot "state\active")) -and (Test-Path -LiteralPath $arpKey)
    }
    New-Item -ItemType Directory -Path $settingsRoot -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $settingsRoot "settings.toml"), "preserve-locked-residual")
    $lockedStateExecutable = Join-Path $settingsRoot "locked-state.exe"
    $systemPowerShell = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
    [IO.File]::Copy($systemPowerShell, $lockedStateExecutable, $false)
    $lockedCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("Start-Sleep -Seconds 30"))
    $lockedStateProcess = Start-Process -FilePath $lockedStateExecutable -ArgumentList @(
        "-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", $lockedCommand
    ) -PassThru -WindowStyle Hidden
    try {
        Start-Sleep -Milliseconds 250
        if ($lockedStateProcess.HasExited) {
            throw "Locked-state process exited before uninstall."
        }
        $lockedStateUninstaller = Join-Path $installRoot "uninstall.exe"
        $lockedStateUninstallExit = Start-TestProcess -FilePath $lockedStateUninstaller -Arguments @("/S", "/REMOVE_SETTINGS")
        if ($lockedStateUninstallExit -ne 0) {
            throw "Locked-state uninstall exited with $lockedStateUninstallExit."
        }
        Wait-TestCondition -Description "locked-state uninstall" -Condition {
            -not (Test-Path -LiteralPath $installRoot) -and -not (Test-Path -LiteralPath $arpKey)
        }
        Wait-TestInstallerIdle
        if ($lockedStateProcess.HasExited) {
            throw "Locked-state process did not remain active through uninstall."
        }
        if (-not (Test-Path -LiteralPath $settingsRoot -PathType Container) -or
            -not (Test-Path -LiteralPath $lockedStateExecutable -PathType Leaf)) {
            throw "Locked-state uninstall did not preserve its undeletable settings residual."
        }
        Assert-TestUserPathRestored
        Write-Host "Locked settings residual remained nonblocking."
    } finally {
        if (-not $lockedStateProcess.HasExited) {
            $lockedStateProcess.Kill()
            [void]$lockedStateProcess.WaitForExit(5000)
        }
        $lockedStateProcess.Dispose()
        if (Test-Path -LiteralPath $settingsRoot) {
            Remove-Item -LiteralPath $settingsRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }

    $reparseStateInstallExit = Start-TestProcess -FilePath $modifiedInstaller -Arguments @("/S")
    if ($reparseStateInstallExit -ne 0) {
        throw "Reparse-state test install exited with $reparseStateInstallExit."
    }
    Wait-TestCondition -Description "reparse-state test install" -Condition {
        (Test-Path -LiteralPath (Join-Path $installRoot "state\active")) -and (Test-Path -LiteralPath $arpKey)
    }
    New-Item -ItemType Directory -Path $settingsRoot -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $settingsRoot "settings.toml"), "preserve-reparse-residual")
    $reparseStateTarget = Join-Path $AgentUserProfileRoot "settings-reparse-target"
    New-Item -ItemType Directory -Path $reparseStateTarget | Out-Null
    [IO.File]::WriteAllText((Join-Path $reparseStateTarget "outside.txt"), "preserve-external")
    $reparseStateLink = Join-Path $settingsRoot "external"
    New-Item -ItemType Junction -Path $reparseStateLink -Target $reparseStateTarget | Out-Null
    try {
        $reparseStateUninstaller = Join-Path $installRoot "uninstall.exe"
        $reparseStateUninstallExit = Start-TestProcess -FilePath $reparseStateUninstaller -Arguments @("/S", "/REMOVE_SETTINGS")
        if ($reparseStateUninstallExit -ne 0) {
            throw "Reparse-state uninstall exited with $reparseStateUninstallExit."
        }
        Wait-TestCondition -Description "reparse-state uninstall" -Condition {
            -not (Test-Path -LiteralPath $installRoot) -and -not (Test-Path -LiteralPath $arpKey)
        }
        Wait-TestInstallerIdle
        if ([IO.File]::ReadAllText((Join-Path $settingsRoot "settings.toml")) -cne "preserve-reparse-residual" -or
            [IO.File]::ReadAllText((Join-Path $reparseStateTarget "outside.txt")) -cne "preserve-external") {
            throw "Reparse-state uninstall changed preserved settings or junction-target content."
        }
        Assert-TestUserPathRestored
        Write-Host "Reparse settings residual remained nonblocking."
    } finally {
        if (Test-Path -LiteralPath $reparseStateLink) {
            [IO.Directory]::Delete($reparseStateLink)
        }
        if (Test-Path -LiteralPath $settingsRoot) {
            Remove-Item -LiteralPath $settingsRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $reparseStateTarget) {
            Remove-Item -LiteralPath $reparseStateTarget -Recurse -Force -ErrorAction SilentlyContinue
        }
    }

    # A running old runtime keeps activation pending. The installed launcher
    # promotes the new runtime after the lease exits, then the native helper
    # publishes the matching launcher and removes the obsolete runtime.
    $pendingRoot = Join-Path $OutputDir "native-pending-update"
    $oldPackage = Join-Path $pendingRoot "old-package"
    $newPackage = Join-Path $pendingRoot "new-package"
    $newBuildId = "fedcba987654.3210fedcba98"
    $newDisplayVersion = if ($ReleaseVersion -ceq "local") {
        "$BuildFreshness (local, build $newBuildId)"
    } else {
        $DisplayVersion
    }
    $newLauncher = Join-Path $pendingRoot "new-launcher\app-launcher.exe"
    New-TestIdentityLauncher -Path $newLauncher -Identity $newBuildId
    New-TestHelperPackage -Root $oldPackage -AppLauncher $LauncherExe -Uninstaller $modifiedInstaller
    New-TestHelperPackage -Root $newPackage -AppLauncher $newLauncher -Uninstaller $modifiedInstaller
    $oldInstallExit = Start-TestProcess -FilePath $InstallerHelperExe -Arguments @(
        "install",
        "--install-root", $installRoot,
        "--user-profile-root", $AgentUserProfileRoot,
        "--package-root", $oldPackage,
        "--build-id", $BuildId,
        "--display-version", $DisplayVersion,
        "--numeric-version", $NumericVersion,
        "--install-manager", "Direct"
    )
    if ($oldInstallExit -ne 0) {
        throw "Native helper pending-update fixture install exited with $oldInstallExit."
    }
    $repairLease = Join-Path $installRoot "state\leases\$BuildId.lease"
    [IO.File]::WriteAllText($repairLease, "")
    Remove-Item -LiteralPath (Join-Path $installRoot "state\installer-helper.exe") -Force
    $repairInstallExit = Start-TestProcess -FilePath $InstallerHelperExe -Arguments @(
        "install",
        "--install-root", $installRoot,
        "--user-profile-root", $AgentUserProfileRoot,
        "--package-root", $oldPackage,
        "--build-id", $BuildId,
        "--display-version", $DisplayVersion,
        "--numeric-version", $NumericVersion,
        "--install-manager", "Direct"
    )
    if ($repairInstallExit -ne 0 -or
        -not (Test-Path -LiteralPath (Join-Path $installRoot "state\installer-helper.exe") -PathType Leaf) -or
        -not (Test-Path -LiteralPath $repairLease -PathType Leaf)) {
        throw "Native setup did not narrowly repair a missing installed helper."
    }
    Write-Host "Native missing-helper repair passed."
    $leaseReady = Join-Path $pendingRoot "lease-ready"
    $leaseHolder = Start-TestLeaseHolder `
        -LeasePath (Join-Path $installRoot "state\leases\$BuildId.lease") `
        -ReadyPath $leaseReady
    try {
        Wait-TestCondition -Description "pending-update lease holder" -Condition {
            Test-Path -LiteralPath $leaseReady -PathType Leaf
        }
        $pendingInstallExit = Start-TestProcess -FilePath $InstallerHelperExe -Arguments @(
            "install",
            "--install-root", $installRoot,
            "--user-profile-root", $AgentUserProfileRoot,
            "--package-root", $newPackage,
            "--build-id", $newBuildId,
            "--display-version", $newDisplayVersion,
            "--numeric-version", $NumericVersion,
            "--install-manager", "Direct"
        )
        if ($pendingInstallExit -ne 0) {
            throw "Native helper pending update exited with $pendingInstallExit."
        }
        $activeText = [IO.File]::ReadAllText((Join-Path $installRoot "state\active")).Replace("`r`n", "`n")
        $pendingText = [IO.File]::ReadAllText((Join-Path $installRoot "state\pending")).Replace("`r`n", "`n")
        if ($activeText -cne "herdr-pointer-v1`nbuild_id=$BuildId`n" -or
            $pendingText -cne "herdr-pointer-v1`nbuild_id=$newBuildId`n") {
            throw "Native helper did not preserve active and pending pointer ownership while the old lease was live."
        }
    } finally {
        [void]$leaseHolder.Release.Set()
        if (-not $leaseHolder.Process.WaitForExit(15000)) {
            $leaseHolder.Process.Kill()
            [void]$leaseHolder.Process.WaitForExit(5000)
        }
        $leaseHolder.Process.Dispose()
        $leaseHolder.Release.Dispose()
    }
    $launcherExit = Start-TestProcess -FilePath (Join-Path $installRoot "bin\herdr.exe") -Arguments @("--version")
    if ($launcherExit -ne 0) {
        throw "Installed launcher could not activate the pending runtime; exit code $launcherExit."
    }
    $expectedLauncherHash = Get-TestFileSha256 -Path $newLauncher
    Wait-TestCondition -Description "native pending-update maintenance" -Condition {
        $active = Join-Path $installRoot "state\active"
        try {
            $installedLauncherHash = Get-TestFileSha256 -Path (Join-Path $installRoot "bin\herdr.exe")
        } catch {
            $exception = $_.Exception
            while ($null -ne $exception.InnerException) {
                $exception = $exception.InnerException
            }
            if ($exception -is [IO.IOException]) {
                return $false
            }
            throw
        }
        (Test-Path -LiteralPath $active -PathType Leaf) -and
            ([IO.File]::ReadAllText($active).Replace("`r`n", "`n") -ceq "herdr-pointer-v1`nbuild_id=$newBuildId`n") -and
            -not (Test-Path -LiteralPath (Join-Path $installRoot "state\pending")) -and
            -not (Test-Path -LiteralPath (Join-Path $installRoot "runtime\$BuildId")) -and
            ($installedLauncherHash -ceq $expectedLauncherHash)
    }
    $nativeUninstallArguments = @(
        "uninstall",
        "--install-root", $installRoot,
        "--user-profile-root", $AgentUserProfileRoot,
        "--skill-hash-manifest", (Join-Path $newPackage "skill\managed-skill-hashes.txt"),
        "--settings-disposition", "Keep",
        "--skill-disposition", "Auto"
    )
    $malformedPendingLauncher = Join-Path $installRoot "state\launcher.pending-not-a-hash.exe"
    [IO.File]::WriteAllText($malformedPendingLauncher, "preserve")
    $malformedPendingExit = Start-TestProcess -FilePath $InstallerHelperExe -Arguments $nativeUninstallArguments
    if ($malformedPendingExit -eq 0 -or
        -not (Test-Path -LiteralPath $malformedPendingLauncher -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $installRoot "bin\herdr.exe") -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $installRoot "runtime\$newBuildId") -PathType Container) -or
        -not (Test-Path -LiteralPath $arpKey) -or
        -not (Test-Path -LiteralPath $skillPath -PathType Leaf)) {
        throw "Malformed pending-launcher state did not fail closed before uninstall mutation."
    }
    Remove-Item -LiteralPath $malformedPendingLauncher -Force

    $validArpDisplayVersion = [string](Get-ItemProperty -LiteralPath $arpKey).DisplayVersion
    Set-ItemProperty -LiteralPath $arpKey -Name DisplayVersion -Value "$validArpDisplayVersion.extra"
    $malformedArpExit = Start-TestProcess -FilePath $InstallerHelperExe -Arguments $nativeUninstallArguments
    if ($malformedArpExit -eq 0 -or
        -not (Test-Path -LiteralPath (Join-Path $installRoot "bin\herdr.exe") -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $installRoot "runtime\$newBuildId") -PathType Container) -or
        -not (Test-Path -LiteralPath $arpKey) -or
        -not (Test-Path -LiteralPath $skillPath -PathType Leaf)) {
        throw "Malformed ARP display identity did not fail closed before uninstall mutation."
    }
    Set-ItemProperty -LiteralPath $arpKey -Name DisplayVersion -Value $validArpDisplayVersion

    $nativeUninstallExit = Start-TestProcess -FilePath $InstallerHelperExe -Arguments $nativeUninstallArguments
    if ($nativeUninstallExit -ne 0) {
        throw "Native helper pending-update fixture uninstall exited with $nativeUninstallExit."
    }
    Wait-TestCondition -Description "native pending-update cleanup" -Condition {
        -not (Test-Path -LiteralPath $installRoot) -and -not (Test-Path -LiteralPath $arpKey)
    }
    Assert-TestUserPathRestored
    Write-Host "Native pending-update activation passed."
} catch {
    $testFailure = $_
    throw
} finally {
    try {
        Remove-TestInstallIfPresent
    } catch {
        if ($null -eq $testFailure) {
            throw
        }
        Write-Warning "Test cleanup also failed after the original error: $_"
    }
    if (Test-Path -LiteralPath $skillRoot) {
        Remove-Item -LiteralPath $skillRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $claudeSkillRoot) {
        Remove-Item -LiteralPath $claudeSkillRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $settingsRoot) {
        Remove-Item -LiteralPath $settingsRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    foreach ($fault in $cleanupFaults) {
        $faultMarker = Join-Path $env:TEMP "herdr-uninstall-fault-$fault.once"
        if (Test-Path -LiteralPath $faultMarker) {
            Remove-Item -LiteralPath $faultMarker -Force -ErrorAction SilentlyContinue
        }
        $installFailure = Join-Path $env:TEMP "herdr-install-failure-$fault.txt"
        if (Test-Path -LiteralPath $installFailure) {
            Remove-Item -LiteralPath $installFailure -Force -ErrorAction SilentlyContinue
        }
    }
    foreach ($installFault in @("after-user-path", "after-arp-path-added")) {
        $installFaultMarker = Join-Path $env:TEMP "herdr-uninstall-fault-install-$installFault.once"
        if (Test-Path -LiteralPath $installFaultMarker) {
            Remove-Item -LiteralPath $installFaultMarker -Force
        }
    }
}

Write-Host "Windows installer fault matrix passed."
} finally {
    $env:USERPROFILE = $originalUserProfile
    $env:LOCALAPPDATA = $originalLocalAppData
    $env:CLAUDE_CONFIG_DIR = $originalClaudeConfigDir
    if ($null -eq $originalXdgConfigHome) {
        Remove-Item Env:XDG_CONFIG_HOME -ErrorAction SilentlyContinue
    } else {
        $env:XDG_CONFIG_HOME = $originalXdgConfigHome
    }
    if ($null -eq $originalSession) {
        Remove-Item Env:HERDR_SESSION -ErrorAction SilentlyContinue
    } else {
        $env:HERDR_SESSION = $originalSession
    }
    if ($null -eq $originalSocketPath) {
        Remove-Item Env:HERDR_SOCKET_PATH -ErrorAction SilentlyContinue
    } else {
        $env:HERDR_SOCKET_PATH = $originalSocketPath
    }
    if ($null -eq $originalClientSocketPath) {
        Remove-Item Env:HERDR_CLIENT_SOCKET_PATH -ErrorAction SilentlyContinue
    } else {
        $env:HERDR_CLIENT_SOCKET_PATH = $originalClientSocketPath
    }
    if ($null -eq $originalRemoteSidecar) {
        Remove-Item Env:HERDR_REMOTE_SIDECAR_V1 -ErrorAction SilentlyContinue
    } else {
        $env:HERDR_REMOTE_SIDECAR_V1 = $originalRemoteSidecar
    }
    try {
        Restore-TestUserPath
    } finally {
        Remove-TestOwnedUserProfile
    }
}
