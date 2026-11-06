param([string]$Archive = '')
$ErrorActionPreference = 'Stop'
if (-not $Archive) {
    $Archive = (Get-ChildItem -LiteralPath dist -Filter '*-portable.zip' | Select-Object -First 1).FullName
}
if (-not $Archive) { throw 'Missing portable ZIP' }
$root = Join-Path ([IO.Path]::GetTempPath()) ('torto-portable-' + [Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $root | Out-Null
$process = $null
function Assert($condition, $message) { if (-not $condition) { throw $message } }
function Build-Fixture($output, $source) {
    # PowerShell 7's Add-Type only emits libraries. The Windows PowerShell
    # runtime already depends on .NET Framework, whose compiler can emit EXEs.
    $compiler = Join-Path $env:SystemRoot 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    $file = $output + '.cs'
    [IO.File]::WriteAllText($file, $source)
    & $compiler /nologo /target:winexe ('/out:' + $output) $file
    Assert ($LASTEXITCODE -eq 0) 'Could not compile inert portable update fixture'
}
function Start-Reader($directory) {
    $script:process = Start-Process -FilePath (Join-Path $directory 'torto.exe') -WorkingDirectory $root -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ([DateTime]::UtcNow -lt $deadline) {
        $script:process.Refresh()
        Assert (-not $script:process.HasExited) 'Portable reader failed during startup'
        if (Test-Path -LiteralPath (Join-Path $directory 'Data\config\webdav-sync.json')) { return }
        Start-Sleep -Milliseconds 200
    }
    throw 'Portable startup did not initialize its own settings'
}
function Stop-Reader {
    if ($null -ne $script:process) {
        if (-not $script:process.HasExited) {
            $null = $script:process.CloseMainWindow()
            if (-not $script:process.WaitForExit(10000)) { $script:process.Kill(); $script:process.WaitForExit() }
        }
        $script:process = $null
    }
}
try {
    $portable = Join-Path $root ('Book ' + [char]0x4e66 + ' & spaces')
    Expand-Archive -LiteralPath $Archive -DestinationPath $portable
    $exe = Join-Path $portable 'torto.exe'
    # Verify actual PE imports rather than assuming crt-static affected every
    # C/C++ dependency. Windows 10/11 provide the system UCRT/API-set DLLs.
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $dumpbin = & $vswhere -latest -products '*' -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
    Assert ($null -ne $dumpbin) 'Cannot inspect Windows runtime dependencies'
    $imports = (& $dumpbin /dependents $exe | Out-String)
    Assert ($LASTEXITCODE -eq 0) 'PE import inspection failed'
    Assert ($imports -notmatch '(?i)\b(?:vcruntime|msvcp|concrt)\w*\.dll\b') 'Portable executable requires an external MSVC runtime'

    Start-Reader $portable
    Stop-Reader
    $settings = Get-Content -LiteralPath (Join-Path $portable 'Data\config\webdav-sync.json') -Raw -Encoding UTF8
    $fixture = Join-Path $portable 'Data\config\credential-fixture.txt'
    [IO.File]::WriteAllText($fixture, 'untouched credential fixture')
    $moved = Join-Path $root 'Moved portable'
    Move-Item -LiteralPath $portable -Destination $moved
    Start-Reader $moved
    Stop-Reader
    Assert ((Get-Content -LiteralPath (Join-Path $moved 'Data\config\webdav-sync.json') -Raw -Encoding UTF8) -eq $settings) 'Moving within the same machine changed sync settings'
    Assert ((Get-Content -LiteralPath (Join-Path $moved 'Data\config\credential-fixture.txt') -Raw) -eq 'untouched credential fixture') 'Moving changed profile data'

    # Small inert programs exercise the real detached helper without updating
    # the repository binary or any user installation.
    $old = Join-Path $root 'fixture-old.exe'
    Build-Fixture $old @'
using System;
using System.IO;
public class PortableOldFixture {
    public static void Main(string[] args) {
        File.WriteAllText(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "old-ran"), "yes");
    }
}
'@
    $good = Join-Path $root 'fixture-new.exe'
    Build-Fixture $good @'
using System;
using System.IO;
using System.Text.RegularExpressions;
public class PortableNewFixture {
    public static void Main(string[] args) {
        if (args.Length < 2) return;
        string json = File.ReadAllText(Path.Combine(args[1], "request.json"));
        string token = Regex.Match(json, @"""token""\s*:\s*""([^""]+)""").Groups[1].Value;
        if (String.IsNullOrEmpty(token)) return;
        File.WriteAllText(Path.Combine(args[1], "healthy"), token);
        System.Threading.Thread.Sleep(1000);
    }
}
'@
    foreach ($mode in @('success', 'rollback', 'corrupt')) {
        $directory = Join-Path $root ('update ' + $mode)
        $session = Join-Path $directory 'Data\cache\updates\portable-fixture'
        New-Item -ItemType Directory -Path $session -Force | Out-Null
        $target = Join-Path $directory 'torto.exe'
        Copy-Item -LiteralPath $old -Destination $target
        [IO.File]::WriteAllText((Join-Path $directory 'torto-portable.json'), '{"version":1}')
        $data = Join-Path $directory 'Data\credentials.txt'; [IO.File]::WriteAllText($data, 'saved data')
        $staged = Join-Path $session 'new.exe'
        Copy-Item -LiteralPath $(if ($mode -eq 'success') { $good } else { $old }) -Destination $staged
        $hash = (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($mode -eq 'corrupt') { $hash = '0' * 64 }
        $job = @{target=$target;staged=$staged;backup=(Join-Path $session 'previous.exe');parent=2147483647;sha256=$hash;token=[Guid]::NewGuid().ToString()}
        [IO.File]::WriteAllText((Join-Path $session 'request.json'), ($job | ConvertTo-Json))
        $env:TORTO_UPDATE_JOB = $session
        $helper = Start-Process -FilePath powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',('"' + (Join-Path $PSScriptRoot 'update-windows-portable.ps1') + '"')) -PassThru
        Assert ($helper.WaitForExit(60000)) 'Portable helper timed out'
        $expected = switch ($mode) {'success' {'completed'} 'rollback' {'rolled-back'} 'corrupt' {'failed'}}
        $actual = Get-Content -LiteralPath (Join-Path $session 'state.txt') -Raw
        Assert ($actual -eq $expected) ('Unexpected helper result for ' + $mode + ': expected ' + $expected + ', received ' + $actual)
        $expectedHash = (Get-FileHash -LiteralPath $(if ($mode -eq 'success') {$good} else {$old}) -Algorithm SHA256).Hash
        Assert ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -eq $expectedHash) 'Portable program contents mismatch'
        Assert ((Get-Content -LiteralPath $data -Raw) -eq 'saved data') 'Update changed user data'
        # A successful helper exits before its relaunched fixture. Wait for
        # those short-lived children before deleting their mapped EXE files.
        $children = @(Get-Process -Name torto -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $target })
        foreach ($child in $children) {
            if (-not $child.WaitForExit(10000)) { $child.Kill(); throw 'Portable fixture did not exit' }
        }
    }
    Write-Output 'Portable startup, move, runtime imports, verified update and rollback passed'
} finally {
    Stop-Reader
    Remove-Item Env:TORTO_UPDATE_JOB -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
