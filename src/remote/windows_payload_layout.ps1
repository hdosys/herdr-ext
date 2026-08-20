$script:HerdrConptyPayloadFiles = @(
    'conpty/conpty.dll',
    'conpty/x64/OpenConsole.exe',
    'conpty/arm64/OpenConsole.exe'
)

$script:HerdrPortablePayloadFiles = @(
    'herdr.exe',
    'LICENSE.txt',
    'conpty/herdr-conpty.json',
    'conpty/conpty.dll',
    'conpty/x64/OpenConsole.exe',
    'conpty/arm64/OpenConsole.exe',
    'THIRD-PARTY-NOTICES/Microsoft.Windows.Console.ConPTY-LICENSE.txt',
    'THIRD-PARTY-NOTICES/Microsoft.Windows.Console.ConPTY-NOTICE.md'
)

function Get-HerdrFileSha256 {
    param([Parameter(Mandatory = $true)][string] $Path)

    $stream = [IO.File]::Open(
        $Path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try {
            $hash = $algorithm.ComputeHash($stream)
        } finally {
            $algorithm.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
    return [BitConverter]::ToString($hash).Replace('-', '').ToLowerInvariant()
}

function Assert-HerdrPortablePayload {
    param(
        [Parameter(Mandatory = $true)][string] $Root,
        [switch] $AllowLease
    )

    $rootItem = Get-Item -LiteralPath $Root -Force
    if (-not $rootItem.PSIsContainer -or
        ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "portable payload root is not a plain directory: $Root"
    }

    $rootPrefix = $rootItem.FullName.TrimEnd('\') + '\'
    $actualFiles = @(
        foreach ($item in Get-ChildItem -LiteralPath $rootItem.FullName -Recurse -Force) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "portable payload contains a reparse point: $($item.FullName)"
            }
            if (-not $item.PSIsContainer) {
                $item.FullName.Substring($rootPrefix.Length).Replace('\', '/')
            }
        }
    ) | Sort-Object -CaseSensitive
    $expectedFiles = @($script:HerdrPortablePayloadFiles)
    if ($AllowLease) {
        $expectedFiles += '.lease'
    }
    $expectedFiles = @($expectedFiles | Sort-Object -CaseSensitive)
    if ($actualFiles.Count -ne $expectedFiles.Count -or
        [string]::Join("`n", $actualFiles) -cne [string]::Join("`n", $expectedFiles)) {
        throw "portable payload file set mismatch: $Root"
    }

    $markerPath = Join-Path $rootItem.FullName 'conpty\herdr-conpty.json'
    $marker = Get-Content -LiteralPath $markerPath -Raw | ConvertFrom-Json
    if ([int]$marker.schema_version -ne 1 -or
        [string]$marker.package -cne 'Microsoft.Windows.Console.ConPTY' -or
        [string]$marker.architecture -cne 'x86_64') {
        throw 'portable payload ConPTY ownership marker identity mismatch'
    }
    $markerFiles = @(
        $marker.files.PSObject.Properties | ForEach-Object { [string]$_.Name }
    ) | Sort-Object -CaseSensitive
    $expectedConptyFiles = @($script:HerdrConptyPayloadFiles | Sort-Object -CaseSensitive)
    if ($markerFiles.Count -ne $expectedConptyFiles.Count -or
        [string]::Join("`n", $markerFiles) -cne [string]::Join("`n", $expectedConptyFiles)) {
        throw 'portable payload ConPTY ownership marker file set mismatch'
    }
    foreach ($relative in $script:HerdrConptyPayloadFiles) {
        $property = $marker.files.PSObject.Properties[$relative]
        $expectedHash = if ($null -eq $property) { '' } else { [string]$property.Value }
        if ($expectedHash -cnotmatch '^[0-9a-f]{64}$') {
            throw "portable payload ConPTY ownership hash is invalid: $relative"
        }
        $path = Join-Path $rootItem.FullName $relative.Replace('/', '\')
        $actualHash = Get-HerdrFileSha256 -Path $path
        if ($actualHash -cne $expectedHash) {
            throw "portable payload ConPTY hash mismatch: $relative"
        }
    }
}

function Test-HerdrPortablePayload {
    param(
        [Parameter(Mandatory = $true)][string] $Root,
        [switch] $AllowLease
    )

    try {
        Assert-HerdrPortablePayload -Root $Root -AllowLease:$AllowLease
        return $true
    } catch {
        return $false
    }
}
