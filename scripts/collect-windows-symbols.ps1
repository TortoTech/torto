[CmdletBinding()]
param(
    [string]$BuildDirectory = "target\release",
    [string]$OutputDirectory = "dist\symbols\windows-x86_64"
)

$ErrorActionPreference = "Stop"
$executable = Join-Path $BuildDirectory "torto.exe"
$pdb = Join-Path $BuildDirectory "torto.pdb"
foreach ($file in @($executable, $pdb)) {
    if (-not (Test-Path -LiteralPath $file) -or (Get-Item -LiteralPath $file).Length -eq 0) {
        throw "Required release artifact is missing or empty: $file"
    }
}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
Copy-Item -LiteralPath $pdb -Destination (Join-Path $OutputDirectory "torto.pdb")
$info = [ordered]@{
    commit = $env:GITHUB_SHA
    run = $env:GITHUB_RUN_ID
    target = "x86_64-pc-windows-msvc"
    executable_sha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
    pdb_sha256 = (Get-FileHash -LiteralPath $pdb -Algorithm SHA256).Hash
}
$info | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory "build-info.json") -Encoding utf8NoBOM
