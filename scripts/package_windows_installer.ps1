[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$StageDir,

    [Parameter(Mandatory = $true)]
    [string]$LauncherExe,

    [Parameter(Mandatory = $true)]
    [string]$InstallerHelperExe,

    [Parameter(Mandatory = $true)]
    [string]$BuildId,

    [string]$BuildFreshness,

    [Parameter(Mandatory = $true)]
    [string]$ReleaseVersion,

    [Parameter(Mandatory = $true)]
    [string]$BaseVersion,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath,

    [string]$ProductName = "Herdr",

    [string]$NsisArchive,
    [string]$NsisCacheDir,

    [ValidateSet(
        "after-bin-directory",
        "after-state-directory",
        "after-uninstaller",
        "after-arp-registration",
        "terminate-after-installer-helper"
    )]
    [string]$TestUninstallFault,

    [ValidateSet("after-user-path", "after-arp-path-added")]
    [string]$TestInstallFault,

    [string]$TestUserProfileRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$NsisVersion = "3.12"
$NsisArchiveName = "nsis-$NsisVersion.zip"
$NsisArchiveUrl = "https://downloads.sourceforge.net/project/nsis/NSIS%203/$NsisVersion/$NsisArchiveName"
$NsisArchiveSha256 = "56581f90db321581c5381193d796fffcf2d24b2f8fed2160a6c6a3baa67f2c4f"
$CompanyName = "herdr-win"
$Copyright = "Herdr contributors"
$CommandName = "herdr"
$DistributionName = "Herdr Win"
$ProductUrl = "https://github.com/hdosys/herdr-win"
$UpstreamUrl = "https://github.com/herdrdev/herdr"
$InstallerStartGateEnvironmentVariable = "HERDR_INSTALLER_START_GATE_V1"
$InstallerTestMarkerPrefix = "herdr"
$ProductNamePattern = '^[A-Za-z0-9](?:[A-Za-z0-9 ._-]{0,62}[A-Za-z0-9_-])?$'
$BuildIdPattern = '^[0-9a-f]{12}\.[0-9a-f]{12}$'
$BuildFreshnessPattern = '^(?<year>[0-9]{4})\.(?<month>[0-9]{2})\.(?<day>[0-9]{2})\.(?<hour>[0-9]{2})(?<minute>[0-9]{2})Z$'
$BaseVersionPattern = '^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$'
$ReleaseVersionPattern = '^(?<year>[0-9]{4})\.(?<month>[0-9]{2})\.(?<day>[0-9]{2})\.(?<sequence>[1-9][0-9]*)$'
$LauncherBuildIdArgument = "--herdr-private-launcher-build-id-v1"

function ConvertTo-WindowsCommandLineArgument {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Value)

    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
        return $Value
    }
    $quoted = New-Object Text.StringBuilder
    [void]$quoted.Append('"')
    $backslashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') {
            $backslashes += 1
            continue
        }
        if ($character -eq '"') {
            [void]$quoted.Append((('\' * ($backslashes * 2 + 1)) -join ''))
            [void]$quoted.Append('"')
        } else {
            if ($backslashes -gt 0) {
                [void]$quoted.Append((('\' * $backslashes) -join ''))
            }
            [void]$quoted.Append($character)
        }
        $backslashes = 0
    }
    if ($backslashes -gt 0) {
        [void]$quoted.Append((('\' * ($backslashes * 2)) -join ''))
    }
    [void]$quoted.Append('"')
    return $quoted.ToString()
}

function Stop-OwnedProcessTree {
    param([Parameter(Mandatory = $true)][Diagnostics.Process]$Process)

    $cleanup = [Diagnostics.Stopwatch]::StartNew()
    $taskkillPath = Join-Path $env:SystemRoot "System32\taskkill.exe"
    $taskkillItem = Get-Item -LiteralPath $taskkillPath -Force
    if ($taskkillItem.PSIsContainer -or
        ($taskkillItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $taskkillItem.Length -le 0) {
        throw "Build-tool cleanup could not validate the Windows process-tree terminator."
    }
    $termination = Start-Process -FilePath $taskkillPath `
        -ArgumentList @("/PID", [string]$Process.Id, "/T", "/F") `
        -WindowStyle Hidden `
        -PassThru
    try {
        if (-not $termination.WaitForExit(2500)) {
            try {
                $termination.Kill()
            } catch {
            }
            $remaining = [Math]::Max(0, 5000 - [int]$cleanup.ElapsedMilliseconds)
            if (-not $termination.WaitForExit($remaining)) {
                throw "Build-tool process-tree termination exceeded its 5-second cleanup limit."
            }
        }
        $remaining = [Math]::Max(0, 5000 - [int]$cleanup.ElapsedMilliseconds)
        if (-not $Process.WaitForExit($remaining)) {
            throw "Build-tool process-tree termination did not reap the root within 5 seconds."
        }
    } finally {
        $termination.Dispose()
        $cleanup.Stop()
    }
}

function Invoke-NativeCaptured {
    param(
        [Parameter(Mandatory = $true)][string]$Command,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [int]$TimeoutSeconds = 180
    )

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Command
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    # Identity probes are new direct processes, not continuations of a remote
    # sidecar. Do not let the caller's private remote mode require a package-local
    # sidecar lease that does not belong to the validated installer stage.
    [void]$startInfo.EnvironmentVariables.Remove("HERDR_REMOTE_SIDECAR_V1")
    if ($null -ne $startInfo.PSObject.Properties["ArgumentList"]) {
        foreach ($argument in $Arguments) {
            [void]$startInfo.ArgumentList.Add($argument)
        }
    } else {
        $startInfo.Arguments = (@($Arguments | ForEach-Object {
            ConvertTo-WindowsCommandLineArgument -Value $_
        }) -join ' ')
    }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "Could not start $Command"
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            Stop-OwnedProcessTree -Process $process
            throw "$Command exceeded its $TimeoutSeconds second packaging timeout."
        }
        if (-not [Threading.Tasks.Task]::WaitAll(
            [Threading.Tasks.Task[]]@($stdout, $stderr),
            5000
        )) {
            throw "$Command exited without closing captured packaging output."
        }
        return [PSCustomObject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.Result
            Stderr = $stderr.Result
        }
    } finally {
        $process.Dispose()
    }
}

function Invoke-NativeChecked {
    param(
        [Parameter(Mandatory = $true)][string]$Command,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [int]$TimeoutSeconds = 180
    )

    $result = Invoke-NativeCaptured -Command $Command -Arguments $Arguments -TimeoutSeconds $TimeoutSeconds
    if (-not [string]::IsNullOrEmpty($result.Stdout)) {
        [Console]::Out.Write($result.Stdout)
    }
    if (-not [string]::IsNullOrEmpty($result.Stderr)) {
        [Console]::Error.Write($result.Stderr)
    }
    if ($result.ExitCode -ne 0) {
        throw "$Command failed with exit code $($result.ExitCode)"
    }
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($sha256.ComputeHash($stream))).Replace("-", "").ToLowerInvariant()
    } finally {
        $sha256.Dispose()
        $stream.Dispose()
    }
}

function Get-PeMachine {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $reader = New-Object System.IO.BinaryReader($stream)
    try {
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) {
            throw "$Path is not a PE executable."
        }
        $stream.Position = 0x3C
        $peOffset = $reader.ReadUInt32()
        if ($peOffset + 6 -gt $stream.Length) {
            throw "$Path has an invalid PE header offset."
        }
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) {
            throw "$Path has an invalid PE signature."
        }
        return $reader.ReadUInt16()
    } finally {
        $reader.Dispose()
        $stream.Dispose()
    }
}

function Assert-X64Pe {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required executable does not exist: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw "Refusing a reparse-point executable input: $Path"
    }
    $machine = Get-PeMachine -Path $Path
    if ($machine -ne 0x8664) {
        throw "Expected an x86_64 PE executable at $Path, got machine 0x$($machine.ToString('x4'))."
    }
}

function Assert-X86Pe {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required executable does not exist: $Path"
    }
    $machine = Get-PeMachine -Path $Path
    if ($machine -ne 0x014C) {
        throw "Expected an x86 PE executable at $Path, got machine 0x$($machine.ToString('x4'))."
    }
}

function Get-VersionIdentity {
    if ($BaseVersion -cnotmatch $BaseVersionPattern) {
        throw "BaseVersion must be the upstream Herdr semantic version without v."
    }
    if ($ReleaseVersion -ceq "local") {
        $freshnessMatch = [regex]::Match($BuildFreshness, $BuildFreshnessPattern)
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
        $timeComponent = [int]$freshnessMatch.Groups['hour'].Value * 100 + [int]$freshnessMatch.Groups['minute'].Value
        return [PSCustomObject]@{
            Freshness = $BuildFreshness
            Display = "$BuildFreshness (local, build $BuildId)"
            Numeric = "$([int]$freshnessMatch.Groups['year'].Value).$([int]$freshnessMatch.Groups['month'].Value).$([int]$freshnessMatch.Groups['day'].Value).$timeComponent"
            Ui = $BuildFreshness
            ExpectedCli = "herdr-win $BuildFreshness (local, Herdr $BaseVersion, build $BuildId)"
        }
    }
    $match = [regex]::Match($ReleaseVersion, $ReleaseVersionPattern)
    if (-not $match.Success) {
        throw "ReleaseVersion must be 'local' or YYYY.MM.DD.N CalVer."
    }
    $date = [DateTime]::MinValue
    $dateText = "$($match.Groups['year'].Value)-$($match.Groups['month'].Value)-$($match.Groups['day'].Value)"
    if (-not [DateTime]::TryParseExact(
        $dateText,
        "yyyy-MM-dd",
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::None,
        [ref]$date
    )) {
        throw "ReleaseVersion must contain a real calendar date."
    }
    $sequence = [uint64]$match.Groups['sequence'].Value
    if ($sequence -gt 65535) {
        throw "ReleaseVersion sequence must fit the Windows 0-65535 version component."
    }
    return [PSCustomObject]@{
        Freshness = $ReleaseVersion
        Display = $ReleaseVersion
        Numeric = "$([int]$match.Groups['year'].Value).$([int]$match.Groups['month'].Value).$([int]$match.Groups['day'].Value).$sequence"
        Ui = $ReleaseVersion
        ExpectedCli = "herdr-win $ReleaseVersion (Herdr $BaseVersion)"
    }
}

function Invoke-HerdrIdentityQuery {
    param(
        [Parameter(Mandatory = $true)][string]$Executable,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$ExpectedOutput,
        [Parameter(Mandatory = $true)][string]$Description,
        [int]$TimeoutSeconds = 30
    )

    $result = Invoke-NativeCaptured `
        -Command $Executable `
        -Arguments $Arguments `
        -TimeoutSeconds $TimeoutSeconds
    if ($result.ExitCode -ne 0) {
        throw "$Description failed with exit code $($result.ExitCode): $($result.Stdout)$($result.Stderr)"
    }
    if ($result.Stderr.Length -ne 0) {
        throw "$Description wrote unexpected stderr: $($result.Stderr)"
    }
    $output = if ($result.Stdout.EndsWith("`r`n", [StringComparison]::Ordinal)) {
        $result.Stdout.Substring(0, $result.Stdout.Length - 2)
    } elseif ($result.Stdout.EndsWith("`n", [StringComparison]::Ordinal)) {
        $result.Stdout.Substring(0, $result.Stdout.Length - 1)
    } else {
        $result.Stdout
    }
    if ($output -cne $ExpectedOutput) {
        throw "$Description returned '$output'; expected exact output '$ExpectedOutput'."
    }
    return $output
}

function Get-VerifiedNsisArchive {
    if ([string]::IsNullOrWhiteSpace($script:NsisArchive)) {
        if ([string]::IsNullOrWhiteSpace($script:NsisCacheDir)) {
            $cacheBase = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TOOL_CACHE)) {
                Join-Path ([System.IO.Path]::GetTempPath()) "herdr-tools"
            } else {
                $env:RUNNER_TOOL_CACHE
            }
            $script:NsisCacheDir = Join-Path $cacheBase "nsis-$script:NsisVersion"
        }
        if (-not (Test-Path -LiteralPath $script:NsisCacheDir)) {
            New-Item -ItemType Directory -Path $script:NsisCacheDir -Force | Out-Null
        }
        $script:NsisArchive = Join-Path $script:NsisCacheDir $script:NsisArchiveName
        if (-not (Test-Path -LiteralPath $script:NsisArchive -PathType Leaf)) {
            $download = "$script:NsisArchive.download.$([Guid]::NewGuid().ToString('N'))"
            try {
                $curl = Get-Command curl.exe -ErrorAction Stop
                Invoke-NativeChecked $curl.Source @(
                    "--fail",
                    "--location",
                    "--max-time", "120",
                    "--silent",
                    "--show-error",
                    "--output", $download,
                    $script:NsisArchiveUrl
                ) -TimeoutSeconds 130
                $downloadHash = Get-Sha256 -Path $download
                if ($downloadHash -cne $script:NsisArchiveSha256) {
                    throw "Downloaded NSIS $script:NsisVersion archive hash mismatch: expected $script:NsisArchiveSha256, got $downloadHash"
                }
                Move-Item -LiteralPath $download -Destination $script:NsisArchive
            } finally {
                if (Test-Path -LiteralPath $download) {
                    Remove-Item -LiteralPath $download -Force
                }
            }
        }
    }

    $archivePath = (Resolve-Path -LiteralPath $script:NsisArchive).Path
    $archiveItem = Get-Item -LiteralPath $archivePath -Force
    if ($archiveItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw "Refusing a reparse-point NSIS archive: $archivePath"
    }
    $actualHash = Get-Sha256 -Path $archivePath
    if ($actualHash -cne $script:NsisArchiveSha256) {
        throw "NSIS $script:NsisVersion archive hash mismatch: expected $script:NsisArchiveSha256, got $actualHash"
    }
    return $archivePath
}

if ($BuildId -cnotmatch $BuildIdPattern) {
    throw "Invalid build ID '$BuildId'. Expected 12 lowercase hex characters, a dot, and 12 lowercase hex characters."
}
if ($ProductName -cnotmatch $ProductNamePattern) {
    throw "Invalid product name '$ProductName'."
}
if (-not [string]::IsNullOrWhiteSpace($TestUserProfileRoot)) {
    $TestUserProfileRoot = (Resolve-Path -LiteralPath $TestUserProfileRoot).Path
    if (-not (Test-Path -LiteralPath $TestUserProfileRoot -PathType Container) -or
        (Get-Item -LiteralPath $TestUserProfileRoot -Force).Attributes -band [System.IO.FileAttributes]::ReparsePoint -or
        $TestUserProfileRoot -match '[\r\n"$]') {
        throw "TestUserProfileRoot must be a regular NSIS-safe directory: $TestUserProfileRoot"
    }
}
$versionIdentity = Get-VersionIdentity
$BuildFreshness = [string]$versionIdentity.Freshness
$DisplayVersion = [string]$versionIdentity.Display
$NumericVersion = [string]$versionIdentity.Numeric
$UiVersion = [string]$versionIdentity.Ui
$ExpectedCliVersion = [string]$versionIdentity.ExpectedCli

$projectRoot = Split-Path -Parent $PSScriptRoot
$packager = Join-Path $PSScriptRoot "package_windows_conpty.py"
$installerScript = Join-Path $projectRoot "packaging\windows\installer\project.nsi"
$installerValidator = Join-Path $projectRoot "packaging\windows\installer\validate-build.ps1"
$skillSource = Join-Path $projectRoot "skills\herdr\SKILL.md"
$skillHashManifest = Join-Path $projectRoot "packaging\windows\managed-skill-hashes.txt"
$artworkDir = Join-Path $projectRoot "packaging\windows\installer\artwork"
$artworkFiles = @(
    "installer-welcome-finish-164x314.bmp",
    "installer-welcome-finish-205x393.bmp",
    "installer-welcome-finish-246x471.bmp",
    "installer-welcome-finish-287x550.bmp",
    "installer-welcome-finish-328x628.bmp"
)
$StageDir = (Resolve-Path -LiteralPath $StageDir).Path
$LauncherExe = (Resolve-Path -LiteralPath $LauncherExe).Path
$InstallerHelperExe = (Resolve-Path -LiteralPath $InstallerHelperExe).Path
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath)
$InstallerOriginalFilename = [System.IO.Path]::GetFileName($OutputPath)

if (-not (Test-Path -LiteralPath $StageDir -PathType Container) -or
    (Get-Item -LiteralPath $StageDir -Force).Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
    throw "Stage must be a regular directory: $StageDir"
}
if (Test-Path -LiteralPath $OutputPath) {
    $existingOutput = Get-Item -LiteralPath $OutputPath -Force
    if ($existingOutput.PSIsContainer -or
        ($existingOutput.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Installer output is not a replaceable regular file: $OutputPath"
    }
}
$requiredSources = @(
    $packager,
    $installerScript,
    $installerValidator,
    $skillSource,
    $skillHashManifest
)
foreach ($artworkFile in $artworkFiles) {
    $requiredSources += Join-Path $artworkDir $artworkFile
}
foreach ($required in $requiredSources) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required installer source does not exist: $required"
    }
    if ((Get-Item -LiteralPath $required -Force).Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw "Refusing a reparse-point installer source: $required"
    }
}
$skillText = (New-Object Text.UTF8Encoding($false, $true)).GetString([IO.File]::ReadAllBytes($skillSource))
$skillValidationText = $skillText.Replace("`r`n", "`n")
if ($skillValidationText.Contains("`r") -or
    -not $skillValidationText.StartsWith("---`n", [StringComparison]::Ordinal) -or
    $skillValidationText -cnotmatch '(?m)^name: herdr$') {
    throw "skills/herdr/SKILL.md is not the canonical Herdr agent skill."
}
$canonicalSkillBytes = (New-Object Text.UTF8Encoding($false)).GetBytes($skillValidationText)
$canonicalSkillHasher = [Security.Cryptography.SHA256]::Create()
try {
    $canonicalSkillHash = ([BitConverter]::ToString(
        $canonicalSkillHasher.ComputeHash($canonicalSkillBytes)
    )).Replace("-", "").ToLowerInvariant()
} finally {
    $canonicalSkillHasher.Dispose()
}
$skillHashManifestText = (New-Object Text.UTF8Encoding($false, $true)).GetString([IO.File]::ReadAllBytes($skillHashManifest))
$skillHashManifestValidationText = $skillHashManifestText.Replace("`r`n", "`n")
if ($skillHashManifestValidationText.Contains("`r") -or
    -not $skillHashManifestValidationText.EndsWith("`n", [StringComparison]::Ordinal)) {
    throw "Managed skill hash manifest must use valid line endings and end with a newline."
}
$skillHashManifestLines = @($skillHashManifestValidationText.Substring(0, $skillHashManifestValidationText.Length - 1) -split "`n")
if ($skillHashManifestLines.Count -lt 2 -or $skillHashManifestLines[0] -cne "herdr-managed-skill-hashes-v1") {
    throw "Managed skill hash manifest has an invalid header."
}
$managedSkillHashes = @($skillHashManifestLines[1..($skillHashManifestLines.Count - 1)])
$previousManagedSkillHash = $null
foreach ($managedSkillHash in $managedSkillHashes) {
    if ($managedSkillHash -cnotmatch '^[0-9a-f]{64}$' -or
        ($null -ne $previousManagedSkillHash -and [StringComparer]::Ordinal.Compare($previousManagedSkillHash, $managedSkillHash) -ge 0)) {
        throw "Managed skill hashes must be lowercase SHA-256 values in unique sorted order."
    }
    $previousManagedSkillHash = $managedSkillHash
}
if ($managedSkillHashes -cnotcontains $canonicalSkillHash) {
    throw "Canonical skills/herdr/SKILL.md hash is absent from the managed skill hash manifest."
}
Assert-X64Pe -Path $LauncherExe
Assert-X64Pe -Path $InstallerHelperExe
$payloadExe = Join-Path $StageDir "herdr.exe"
Assert-X64Pe -Path $payloadExe
if ($LauncherExe.Equals($payloadExe, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "LauncherExe must be the separately built launcher, not the staged Herdr payload."
}
if ($InstallerHelperExe.Equals($payloadExe, [System.StringComparison]::OrdinalIgnoreCase) -or
    $InstallerHelperExe.Equals($LauncherExe, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "InstallerHelperExe must be the separately built native installer helper."
}
[void](Invoke-HerdrIdentityQuery `
    -Executable $payloadExe `
    -Arguments @("--version") `
    -ExpectedOutput $ExpectedCliVersion `
    -Description "Staged Herdr --version")
[void](Invoke-HerdrIdentityQuery `
    -Executable $LauncherExe `
    -Arguments @($LauncherBuildIdArgument) `
    -ExpectedOutput $BuildId `
    -Description "Herdr launcher private build-ID query")

# The ConPTY packager remains the sole owner of package provenance, hashes,
# exact marker content, and the allowed stage layout.
Invoke-NativeChecked python @(
    $packager,
    "validate",
    "--stage-dir", $StageDir
) -TimeoutSeconds 120

$archivePath = Get-VerifiedNsisArchive
$outputParent = Split-Path -Parent $OutputPath
if (-not (Test-Path -LiteralPath $outputParent -PathType Container)) {
    New-Item -ItemType Directory -Path $outputParent -Force | Out-Null
}
$outputParentItem = Get-Item -LiteralPath $outputParent -Force
if (-not $outputParentItem.PSIsContainer -or
    ($outputParentItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw "Installer output parent is not a regular directory: $outputParent"
}

$workingRoot = Join-Path $outputParent (".$InstallerOriginalFilename.build-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $workingRoot | Out-Null
$workingRootItem = Get-Item -LiteralPath $workingRoot -Force
if (-not $workingRootItem.PSIsContainer -or
    ($workingRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw "Private installer build directory is unsafe: $workingRoot"
}
try {
    $canonicalSkillSource = Join-Path $workingRoot "SKILL.md"
    [IO.File]::WriteAllBytes($canonicalSkillSource, $canonicalSkillBytes)
    $canonicalSkillHashManifest = Join-Path $workingRoot "managed-skill-hashes.txt"
    $canonicalSkillHashManifestBytes = (New-Object Text.UTF8Encoding($false)).GetBytes($skillHashManifestValidationText)
    [IO.File]::WriteAllBytes($canonicalSkillHashManifest, $canonicalSkillHashManifestBytes)
    $definitionPath = Join-Path $workingRoot "installer-build.json"
    $temporaryOutput = Join-Path $workingRoot $InstallerOriginalFilename
    $backupOutput = Join-Path $workingRoot ("previous-" + $InstallerOriginalFilename)
    $definition = [ordered]@{
        Schema = 2
        StageDir = $StageDir
        LauncherExe = $LauncherExe
        InstallerHelperExe = $InstallerHelperExe
        SkillSource = $canonicalSkillSource
        SkillHashManifest = $canonicalSkillHashManifest
        ArtworkDir = $artworkDir
        InstallerScript = $installerScript
        BuildId = $BuildId
        BuildFreshness = $BuildFreshness
        ReleaseVersion = $ReleaseVersion
        BaseVersion = $BaseVersion
        DisplayVersion = $DisplayVersion
        NumericVersion = $NumericVersion
        UiVersion = $UiVersion
        ExpectedCliVersion = $ExpectedCliVersion
        OutputPath = $temporaryOutput
        OriginalFilename = $InstallerOriginalFilename
        ProductName = $ProductName
        DistributionName = $DistributionName
        CompanyName = $CompanyName
        Copyright = $Copyright
        ProductUrl = $ProductUrl
        UpstreamUrl = $UpstreamUrl
        CommandName = $CommandName
        StartGateEnvironmentVariable = $InstallerStartGateEnvironmentVariable
        TestMarkerPrefix = $InstallerTestMarkerPrefix
    }
    [IO.File]::WriteAllText(
        $definitionPath,
        ($definition | ConvertTo-Json -Depth 3),
        (New-Object Text.UTF8Encoding($false))
    )
    Invoke-NativeChecked powershell.exe @(
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy", "Bypass",
        "-File", $installerValidator,
        "-DefinitionPath", $definitionPath
    ) -TimeoutSeconds 60
    $toolRoot = Join-Path $workingRoot "tool"
    Expand-Archive -LiteralPath $archivePath -DestinationPath $toolRoot
    $makensis = Join-Path $toolRoot "nsis-$NsisVersion\makensis.exe"
    if (-not (Test-Path -LiteralPath $makensis -PathType Leaf)) {
        throw "Verified NSIS archive did not contain nsis-$NsisVersion\makensis.exe"
    }
    $versionResult = Invoke-NativeCaptured -Command $makensis -Arguments @("/VERSION") -TimeoutSeconds 30
    $reportedVersion = $versionResult.Stdout.Trim()
    if ($versionResult.ExitCode -ne 0 -or $reportedVersion -cne "v$NsisVersion") {
        throw "Verified makensis reported '$reportedVersion' instead of v$NsisVersion."
    }

    $makensisArguments = @(
        "/WX",
        "/V2",
        "/NOCONFIG",
        "/INPUTCHARSET", "UTF8",
        "/DARG_STAGE_DIR=$StageDir",
        "/DARG_LAUNCHER_EXE=$LauncherExe",
        "/DARG_HELPER_EXE=$InstallerHelperExe",
        "/DARG_SKILL_MD=$canonicalSkillSource",
        "/DARG_SKILL_HASH_MANIFEST=$canonicalSkillHashManifest",
        "/DARG_ARTWORK_DIR=$artworkDir",
        "/DINFO_PRODUCTNAME=$ProductName",
        "/DINFO_DISTRIBUTIONNAME=$DistributionName",
        "/DINFO_COMPANYNAME=$CompanyName",
        "/DINFO_COPYRIGHT=$Copyright",
        "/DINFO_PRODUCTURL=$ProductUrl",
        "/DINFO_UPSTREAMURL=$UpstreamUrl",
        "/DINFO_COMMANDNAME=$CommandName",
        "/DINFO_ORIGINALFILENAME=$InstallerOriginalFilename",
        "/DAPP_BUILD_ID=$BuildId",
        "/DINFO_PRODUCTVERSION_DISPLAY=$DisplayVersion",
        "/DINFO_PRODUCTVERSION_FIXED=$NumericVersion",
        "/DINFO_PRODUCTVERSION_UI=$UiVersion",
        "/DINFO_UPSTREAMVERSION=$BaseVersion",
        "/DAPP_OUTPUT_PATH=$temporaryOutput",
        "/DAPP_START_GATE_ENV=$InstallerStartGateEnvironmentVariable",
        "/DAPP_TEST_MARKER_PREFIX=$InstallerTestMarkerPrefix",
        $installerScript
    )
    if (-not [string]::IsNullOrWhiteSpace($TestUninstallFault)) {
        $makensisArguments = @(
            "/DTEST_UNINSTALL_FAULT=$TestUninstallFault"
        ) + $makensisArguments
    }
    if (-not [string]::IsNullOrWhiteSpace($TestInstallFault)) {
        $makensisArguments = @(
            "/DTEST_INSTALL_FAULT=$TestInstallFault"
        ) + $makensisArguments
    }
    if (-not [string]::IsNullOrWhiteSpace($TestUserProfileRoot)) {
        $makensisArguments = @(
            "/DTEST_USER_PROFILE_ROOT=$TestUserProfileRoot"
        ) + $makensisArguments
    }
    Invoke-NativeChecked $makensis $makensisArguments -TimeoutSeconds 180
    Assert-X86Pe -Path $temporaryOutput
    if (Test-Path -LiteralPath $OutputPath) {
        $existingOutput = Get-Item -LiteralPath $OutputPath -Force
        if ($existingOutput.PSIsContainer -or
            ($existingOutput.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Installer output changed into an unsafe path before publication: $OutputPath"
        }
        [IO.File]::Replace($temporaryOutput, $OutputPath, $backupOutput)
        Remove-Item -LiteralPath $backupOutput -Force
    } else {
        [IO.File]::Move($temporaryOutput, $OutputPath)
    }
} finally {
    Remove-Item -LiteralPath $workingRoot -Recurse -Force -ErrorAction SilentlyContinue
}

$artifact = Get-Item -LiteralPath $OutputPath -Force
if ($artifact.PSIsContainer -or
    ($artifact.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
    $artifact.Length -le 0) {
    throw "Published installer artifact is not a nonempty regular file: $OutputPath"
}
([ordered]@{
    artifact = $artifact.FullName
    bytes = [long]$artifact.Length
    sha256 = Get-Sha256 -Path $artifact.FullName
    nsis = "v$NsisVersion"
}).GetEnumerator() | ForEach-Object { "{0}={1}" -f $_.Key, $_.Value }
