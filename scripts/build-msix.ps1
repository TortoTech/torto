[CmdletBinding()]
param(
    [string]$OutputDirectory = "dist",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$metadata = cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $repoRoot "Cargo.toml") | ConvertFrom-Json
$package = $metadata.packages | Where-Object { $_.name -eq "rebook-desktop" } | Select-Object -First 1
if (-not $package) {
    throw "Could not find the rebook-desktop package in Cargo metadata."
}

$versionParts = $package.version.Split(".")
if ($versionParts.Count -ne 3) {
    throw "Expected a semantic version with three parts, got '$($package.version)'."
}
$major = [int]$versionParts[0]
$minor = [int]$versionParts[1]
$patch = [int]$versionParts[2]
if ($minor -ge 1000 -or $patch -ge 1000) {
    throw "MSIX version mapping requires minor and patch versions below 1000."
}
$msixBuild = ($minor * 1000) + $patch
if ($msixBuild -gt 65535 -or $major -ge 65535) {
    throw "Version '$($package.version)' cannot be represented as an MSIX version."
}
$msixVersion = "1.$major.$msixBuild.0"

if (-not $SkipBuild) {
    cargo build --locked --release --package rebook-desktop --manifest-path (Join-Path $repoRoot "Cargo.toml")
}

$executable = Join-Path $repoRoot "target\release\torto.exe"
if (-not (Test-Path -LiteralPath $executable)) {
    throw "Release executable was not found at '$executable'."
}

$staging = [System.IO.Path]::GetFullPath((Join-Path $repoRoot "target\msix\x64"))
$expectedStagingRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot "target\msix"))
if (-not $staging.StartsWith($expectedStagingRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to recreate unexpected staging path '$staging'."
}
if (Test-Path -LiteralPath $staging) {
    Remove-Item -LiteralPath $staging -Recurse -Force
}
$assetsDirectory = Join-Path $staging "Assets"
New-Item -ItemType Directory -Path $assetsDirectory -Force | Out-Null

Copy-Item -LiteralPath $executable -Destination (Join-Path $staging "torto.exe")
$storeAssets = Join-Path $repoRoot "assets\windows\store"
foreach ($asset in @("StoreLogo.png", "Square44x44Logo.png", "Square150x150Logo.png")) {
    $source = Join-Path $storeAssets $asset
    if (-not (Test-Path -LiteralPath $source)) {
        throw "Store asset was not found at '$source'. Run the generate_windows_icons example first."
    }
    Copy-Item -LiteralPath $source -Destination (Join-Path $assetsDirectory $asset)
}

$manifestTemplate = Get-Content -LiteralPath (Join-Path $repoRoot "apps\desktop\msix\AppxManifest.xml") -Raw
$manifest = $manifestTemplate.Replace("{{VERSION}}", $msixVersion)
Set-Content -LiteralPath (Join-Path $staging "AppxManifest.xml") -Value $manifest -Encoding utf8NoBOM

$kitsBin = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
$makeAppx = Get-ChildItem -LiteralPath $kitsBin -Directory |
    Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
    Sort-Object { [version]$_.Name } -Descending |
    ForEach-Object { Join-Path $_.FullName "x64\makeappx.exe" } |
    Where-Object { Test-Path -LiteralPath $_ } |
    Select-Object -First 1
if (-not $makeAppx) {
    throw "MakeAppx.exe was not found. Install the Windows SDK."
}

$resolvedOutputDirectory = if ([System.IO.Path]::IsPathRooted($OutputDirectory)) {
    [System.IO.Path]::GetFullPath($OutputDirectory)
} else {
    [System.IO.Path]::GetFullPath((Join-Path $repoRoot $OutputDirectory))
}
New-Item -ItemType Directory -Path $resolvedOutputDirectory -Force | Out-Null
$packagePath = Join-Path $resolvedOutputDirectory "Torto-$($package.version)-x64.msix"
& $makeAppx pack /d $staging /p $packagePath /o
if ($LASTEXITCODE -ne 0) {
    throw "MakeAppx failed with exit code $LASTEXITCODE."
}

Write-Host "Built $packagePath"
Write-Host "Application version: $($package.version); MSIX version: $msixVersion"
