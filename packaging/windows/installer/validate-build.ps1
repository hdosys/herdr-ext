param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$DefinitionPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

function Assert-ExactProperties {
    param(
        [Parameter(Mandatory = $true)]$Value,
        [Parameter(Mandatory = $true)][string[]]$Expected
    )

    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $wanted = @($Expected | Sort-Object)
    if ([string]::Join("`n", $actual) -cne [string]::Join("`n", $wanted)) {
        throw "Installer build definition has an unexpected schema."
    }
}

function Assert-SafeInstallerText {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Value,
        [int]$MaximumLength = 260
    )

    if ([string]::IsNullOrWhiteSpace($Value) -or $Value.Length -gt $MaximumLength) {
        throw "$Name must be nonempty and bounded."
    }
    if ($Value.IndexOf('$') -ge 0 -or $Value.IndexOf('"') -ge 0) {
        throw "$Name contains a character that cannot be injected safely into NSIS."
    }
    foreach ($character in $Value.ToCharArray()) {
        if ([int][char]$character -lt 32) {
            throw "$Name contains a control character."
        }
    }
}

function Assert-SafeWindowsLeaf {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Value
    )

    if ([string]::IsNullOrWhiteSpace($Value) -or $Value.Length -gt 120 -or
        $Value -cne $Value.Trim() -or $Value.EndsWith('.') -or
        $Value -ceq '.' -or $Value -ceq '..' -or
        $Value.IndexOfAny([IO.Path]::GetInvalidFileNameChars()) -ge 0 -or
        $Value.IndexOf(';') -ge 0 -or $Value.IndexOf('$') -ge 0) {
        throw "$Name is not a safe bounded Windows leaf."
    }
    $baseName = $Value.Split('.')[0].ToUpperInvariant()
    $reserved = @('CON', 'PRN', 'AUX', 'NUL', 'CLOCK$', 'CONIN$', 'CONOUT$')
    $reserved += 1..9 | ForEach-Object { 'COM' + $_ }
    $reserved += 1..9 | ForEach-Object { 'LPT' + $_ }
    if ($reserved -contains $baseName) {
        throw "$Name uses reserved Windows device name '$baseName'."
    }
}

function Assert-RegularFile {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Path
    )

    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) -or
        $item.Length -le 0) {
        throw "$Name is not a nonempty regular file: $Path"
    }
}

function Assert-RegularDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Path
    )

    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or
        (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Name is not a regular directory: $Path"
    }
}

function Assert-Bitmap {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][int]$Width,
        [Parameter(Mandatory = $true)][int]$Height
    )

    Assert-RegularFile -Name ([IO.Path]::GetFileName($Path)) -Path $Path
    $bytes = [IO.File]::ReadAllBytes($Path)
    $rowBytes = [int](([Math]::Floor(($Width * 3 + 3) / 4)) * 4)
    $pixelBytes = $rowBytes * $Height
    if ($bytes.Length -ne 54 + $pixelBytes -or
        [char]$bytes[0] -cne 'B' -or [char]$bytes[1] -cne 'M' -or
        [BitConverter]::ToInt32($bytes, 2) -ne $bytes.Length -or
        [BitConverter]::ToInt32($bytes, 10) -ne 54 -or
        [BitConverter]::ToInt32($bytes, 14) -ne 40 -or
        [BitConverter]::ToInt32($bytes, 18) -ne $Width -or
        [BitConverter]::ToInt32($bytes, 22) -ne $Height -or
        [BitConverter]::ToInt16($bytes, 26) -ne 1 -or
        [BitConverter]::ToInt16($bytes, 28) -ne 24 -or
        [BitConverter]::ToInt32($bytes, 30) -ne 0 -or
        [BitConverter]::ToInt32($bytes, 34) -ne $pixelBytes) {
        throw "Installer bitmap is not exact uncompressed 24-bit BMP3 at ${Width}x${Height}: $Path"
    }
}

Assert-RegularFile -Name DefinitionPath -Path $DefinitionPath
$definitionText = (New-Object Text.UTF8Encoding($false, $true)).GetString(
    [IO.File]::ReadAllBytes($DefinitionPath)
)
$definition = $definitionText | ConvertFrom-Json
$properties = @(
    'Schema', 'StageDir', 'LauncherExe', 'InstallerHelperExe', 'SkillSource',
    'SkillHashManifest', 'ArtworkDir', 'InstallerScript', 'BuildId', 'BuildFreshness',
    'ReleaseVersion', 'BaseVersion', 'DisplayVersion', 'NumericVersion',
    'UiVersion', 'ExpectedCliVersion', 'OutputPath', 'OriginalFilename',
    'ProductName', 'DistributionName', 'CompanyName', 'Copyright', 'ProductUrl',
    'UpstreamUrl', 'CommandName', 'StartGateEnvironmentVariable', 'TestMarkerPrefix'
)
Assert-ExactProperties -Value $definition -Expected $properties
if ([int]$definition.Schema -ne 2) {
    throw 'Installer build definition schema must be 2.'
}

foreach ($name in $properties | Where-Object { $_ -ne 'Schema' }) {
    Assert-SafeInstallerText -Name $name -Value ([string]$definition.$name) -MaximumLength 1024
}
foreach ($name in @('ProductName', 'CommandName', 'OriginalFilename')) {
    Assert-SafeWindowsLeaf -Name $name -Value ([string]$definition.$name)
}
if ([IO.Path]::GetFileName([string]$definition.OutputPath) -cne [string]$definition.OriginalFilename -or
    [IO.Path]::GetExtension([string]$definition.OriginalFilename) -cne '.exe') {
    throw 'OriginalFilename must exactly match the .exe basename of OutputPath.'
}
if ([string]$definition.BuildId -cnotmatch '^[0-9a-f]{12}\.[0-9a-f]{12}$') {
    throw 'BuildId is not the exact herdr-win build identity.'
}
if ([string]$definition.BaseVersion -cnotmatch '^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$') {
    throw 'BaseVersion must be a canonical three-component semantic version.'
}
if ([string]$definition.NumericVersion -cnotmatch '^(\d+)\.(\d+)\.(\d+)\.(\d+)$') {
    throw 'NumericVersion must have exactly four numeric components.'
}
foreach ($component in ([string]$definition.NumericVersion).Split('.')) {
    if ([uint64]$component -gt 65535) {
        throw 'NumericVersion components must fit Windows version metadata.'
    }
}
if ([string]$definition.ReleaseVersion -ceq 'local') {
    $freshnessMatch = [regex]::Match(
        [string]$definition.BuildFreshness,
        '^(?<year>[0-9]{4})\.(?<month>[0-9]{2})\.(?<day>[0-9]{2})\.(?<hour>[0-9]{2})(?<minute>[0-9]{2})Z$'
    )
    $freshness = [DateTime]::MinValue
    if (-not $freshnessMatch.Success -or -not [DateTime]::TryParseExact(
        [string]$definition.BuildFreshness,
        "yyyy.MM.dd.HHmm'Z'",
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::AssumeUniversal -bor [Globalization.DateTimeStyles]::AdjustToUniversal,
        [ref]$freshness
    )) {
        throw 'Local BuildFreshness must be a real UTC YYYY.MM.DD.HHMMZ value.'
    }
    $expectedNumeric = "$([int]$freshnessMatch.Groups['year'].Value).$([int]$freshnessMatch.Groups['month'].Value).$([int]$freshnessMatch.Groups['day'].Value).$([int]$freshnessMatch.Groups['hour'].Value * 100 + [int]$freshnessMatch.Groups['minute'].Value)"
    if ([string]$definition.DisplayVersion -cne ("$($definition.BuildFreshness) (local, build $($definition.BuildId))") -or
        [string]$definition.NumericVersion -cne $expectedNumeric -or
        [string]$definition.UiVersion -cne [string]$definition.BuildFreshness) {
        throw 'Local installer versions are inconsistent.'
    }
} elseif ([string]$definition.BuildFreshness -cne [string]$definition.ReleaseVersion -or
    [string]$definition.DisplayVersion -cne [string]$definition.ReleaseVersion -or
    [string]$definition.UiVersion -cne [string]$definition.ReleaseVersion) {
    throw 'Published installer display and UI versions must equal CalVer.'
}
foreach ($name in @('ProductUrl', 'UpstreamUrl')) {
    $uri = $null
    if (-not [Uri]::TryCreate([string]$definition.$name, [UriKind]::Absolute, [ref]$uri) -or
        $uri.Scheme -cne 'https') {
        throw "$name must be an absolute HTTPS URL."
    }
}

Assert-RegularDirectory -Name StageDir -Path ([string]$definition.StageDir)
Assert-RegularDirectory -Name ArtworkDir -Path ([string]$definition.ArtworkDir)
foreach ($name in @(
    'LauncherExe', 'InstallerHelperExe', 'SkillSource', 'SkillHashManifest', 'InstallerScript'
)) {
    Assert-RegularFile -Name $name -Path ([string]$definition.$name)
}
if ([string]::Equals(
    [string]$definition.LauncherExe,
    [string]$definition.InstallerHelperExe,
    [StringComparison]::OrdinalIgnoreCase
)) {
    throw 'LauncherExe and InstallerHelperExe must be distinct binaries.'
}

$expectedArtwork = @(
    @{ Name = 'installer-welcome-finish-164x314.bmp'; Width = 164; Height = 314 },
    @{ Name = 'installer-welcome-finish-205x393.bmp'; Width = 205; Height = 393 },
    @{ Name = 'installer-welcome-finish-246x471.bmp'; Width = 246; Height = 471 },
    @{ Name = 'installer-welcome-finish-287x550.bmp'; Width = 287; Height = 550 },
    @{ Name = 'installer-welcome-finish-328x628.bmp'; Width = 328; Height = 628 }
)
foreach ($asset in $expectedArtwork) {
    Assert-Bitmap `
        -Path (Join-Path ([string]$definition.ArtworkDir) $asset.Name) `
        -Width $asset.Width `
        -Height $asset.Height
}

[Console]::Out.WriteLine('Herdr Win installer build inputs validated successfully.')
