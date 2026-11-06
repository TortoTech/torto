# Runs in a detached process. All paths come from a verified staged update job.
$ErrorActionPreference = 'Stop'
$directory = $env:TORTO_UPDATE_JOB
$job = Get-Content -LiteralPath (Join-Path $directory 'request.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$root = Split-Path -Parent $job.target
$next = Join-Path $root '.torto-next.exe'
$replaced = $false
$replacement = $null
$lock = $null
function Write-State($stage) {
    [IO.File]::WriteAllText((Join-Path $directory 'state.txt'), $stage)
}
try {
    $parent = Get-Process -Id ([int]$job.parent) -ErrorAction SilentlyContinue
    if ($null -ne $parent -and -not $parent.WaitForExit(120000)) { throw 'Reader did not exit' }
    # Re-check the profile lock after exit, in case another reader was launched.
    $lock = [IO.File]::Open((Join-Path $root 'Data\.instance.lock'), 'OpenOrCreate', 'ReadWrite', 'ReadWrite')
    $lock.Lock(0, 1)
    $digest = (Get-FileHash -LiteralPath $job.staged -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($digest -ne $job.sha256) { throw 'Staged program checksum mismatch' }
    if (-not (Test-Path -LiteralPath (Join-Path $root 'torto-portable.json'))) { throw 'Portable marker is missing' }
    [IO.File]::Copy($job.staged, $next, $true)
    # One filesystem replacement keeps the previous executable for rollback.
    # Failure leaves the installed program in place; never fall back to MSI.
    [IO.File]::Replace($next, $job.target, $job.backup)
    $replaced = $true
    $lock.Dispose(); $lock = $null
    Write-State 'starting'
    $replacement = Start-Process -FilePath $job.target -WorkingDirectory $root -ArgumentList ('--torto-portable-health "' + $directory + '"') -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    $healthy = Join-Path $directory 'healthy'
    while ([DateTime]::UtcNow -lt $deadline) {
        if ((Test-Path -LiteralPath $healthy) -and [IO.File]::ReadAllText($healthy) -eq $job.token) {
            Write-State 'completed'
            $replaced = $false # committed: cleanup failure must not trigger rollback
            Remove-Item -LiteralPath $job.backup -Force -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $job.staged -Force -ErrorAction SilentlyContinue
            exit 0
        }
        $replacement.Refresh()
        if ($replacement.HasExited) { throw 'Updated reader exited before its first frame' }
        Start-Sleep -Milliseconds 100
    }
    throw 'Updated reader did not present a frame'
} catch {
    if ($null -ne $lock) { $lock.Dispose(); $lock = $null }
    if ($replaced) {
        if ($null -ne $replacement -and -not $replacement.HasExited) {
            Stop-Process -Id $replacement.Id -Force -ErrorAction SilentlyContinue
            $replacement.WaitForExit()
        }
        # Windows PowerShell coerces $null to an empty string for this .NET
        # string parameter; File.Replace needs an actual null backup path.
        [IO.File]::Replace($job.backup, $job.target, [NullString]::Value)
        Write-State 'rolled-back'
        Start-Process -FilePath $job.target -WorkingDirectory $root -ArgumentList ('--torto-portable-rollback "' + $directory + '"') | Out-Null
    } else { Write-State 'failed' }
    exit 1
} finally {
    if ($null -ne $lock) { $lock.Dispose() }
    if (Test-Path -LiteralPath $next) { Remove-Item -LiteralPath $next -Force -ErrorAction SilentlyContinue }
}
