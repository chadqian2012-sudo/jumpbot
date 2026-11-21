# Run one bounded round. Resume checkpoint pauses with the worker's configured hotkey.
[CmdletBinding(PositionalBinding=$false)]
param(
    [switch]$ExecuteLive,
    [string]$Exe = (Join-Path $PSScriptRoot 'jumpbot.exe'),
    [Parameter(Mandatory)][string]$Out,
    [ValidateRange(1,2147483647)][int]$Jumps = 500,
    [ValidateRange(1,2147483647)][int]$PauseEvery = 50,
    [ValidateSet('auto','simd','gpu','opencl')][string]$Backend = 'auto',
    [string]$SuiteStopFile
)
$ErrorActionPreference = 'Stop'
if (!$ExecuteLive -or $env:JUMPBOT_NO_INPUT -eq '1') {
    throw 'Requires -ExecuteLive and JUMPBOT_NO_INPUT must not be 1.'
}
Import-Module (Join-Path $PSHOME 'Modules\Microsoft.PowerShell.Utility')
Import-Module (Join-Path $PSHOME 'Modules\CimCmdlets')
$exeFile = Get-Item -LiteralPath $Exe
if ($exeFile.Name -eq 'jumpbot-desktop.exe') {
    $exeFile = Get-Item -LiteralPath (Join-Path $exeFile.DirectoryName 'jumpbot.exe')
}
if ($exeFile.Name -ne 'jumpbot.exe' -or $exeFile.PSIsContainer) { throw 'Expected jumpbot.exe.' }
$app = $exeFile.DirectoryName
$buildPath = Join-Path $app 'build-source.json'
$buildHash = (Get-FileHash -LiteralPath $buildPath).Hash
$build = Get-Content -Encoding UTF8 -LiteralPath $buildPath -Raw | ConvertFrom-Json
$versionPath = Join-Path $app 'source-version.json'
if (Test-Path -LiteralPath $versionPath) {
    $version = Get-Content -Encoding UTF8 -LiteralPath $versionPath -Raw | ConvertFrom-Json
    if ($version.build_manifest_sha256 -ne $buildHash -or $version.commit -ne $build.commit) {
        throw 'Release metadata does not match build-source.json.'
    }
}
$exeHash = (Get-FileHash -LiteralPath $exeFile.FullName).Hash
if ($exeHash -ne $build.files_sha256.'jumpbot.exe') { throw 'Worker executable hash mismatch.' }

# Old Python/frozen versions do not acquire the native worker's single-instance lock.
$owners = Get-CimInstance Win32_Process -Filter "Name='jumpbot.exe' OR Name='python.exe' OR Name='pythonw.exe'" |
    Where-Object { ($_.Name -eq 'jumpbot.exe' -and $_.CommandLine -notmatch ' --panel| -m ') -or
        ($_.Name -ne 'jumpbot.exe' -and $_.CommandLine -match 'jumpbot\.py|live_acceptance\.py' -and
         $_.CommandLine -notmatch '--selftest|--dry-run|--image|--probe') }
if ($owners) { throw "Another JumpBot process may own input (PID $($owners.ProcessId -join ', '))." }

$Out = (New-Item -ItemType Directory -Path $Out).FullName
$statusPath = Join-Path $Out 'test-status.json'
$stopPath = Join-Path $Out 'stop.request'
$logs = Join-Path $Out 'logs'
$state = [ordered]@{state='starting'; requested_jumps=$Jumps; backend=$Backend
    frozen_exe=$exeFile.FullName; frozen_exe_sha256=$exeHash; build_manifest_sha256=$buildHash
    source_commit=$build.commit; child_pid=$null; progress=$null; return_code=$null
    pause_requests=0; round_pass=$false; stats=$null; stop_reason=''}
function Save-Status {
    $tmp = "$statusPath.tmp"
    [IO.File]::WriteAllText($tmp, ($state | ConvertTo-Json -Depth 10))
    if ([IO.File]::Exists($statusPath)) {
        [IO.File]::Replace($tmp, $statusPath, [System.Management.Automation.Language.NullString]::Value)
    } else { [IO.File]::Move($tmp, $statusPath) }
}
function Test-StopRequested {
    [IO.File]::Exists($stopPath) -or ($SuiteStopFile -and [IO.File]::Exists($SuiteStopFile))
}
function Get-RecordedCounts {
    $files = @(Get-ChildItem -LiteralPath $logs -Filter 'run_*.csv' -File)
    if ($files.Count -ne 1) { throw 'Expected one worker CSV for this round.' }
    $counts = [ordered]@{attempts=0; landed=0; deaths=0; hits=0; misses=0; judged=0; unknown=0}
    $index = 0
    foreach ($row in (Import-Csv -LiteralPath $files[0].FullName -Encoding UTF8)) {
        $index++
        if ($row.index -cne [string]$index -or $null -eq $row.center_hit -or
            $row.result -cnotin 'landed','dead','unknown','stopped') { throw "Invalid CSV row $index." }
        if ($row.center_hit -cnotin '','0','1' -or ($row.center_hit -ne '' -and $row.result -ne 'landed')) {
            throw "Invalid score judgement on row $index."
        }
        if ($row.result -eq 'stopped') { continue }
        $counts.attempts++
        if ($row.result -eq 'landed') { $counts.landed++ }
        if ($row.result -eq 'dead') { $counts.deaths++ }
        if ($row.center_hit -eq '1') { $counts.hits++ }
        if ($row.center_hit -eq '0') { $counts.misses++ }
    }
    $counts.judged = $counts.hits + $counts.misses
    $counts.unknown = $counts.attempts - $counts.deaths - $counts.judged
    $counts
}

$child = $null
$console = $stderr = $stderrCopy = $null
try {
    Save-Status
    if (Test-StopRequested) { throw 'User stop before worker launch.' }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $exeFile.FullName
    $info.WorkingDirectory = $app
    $info.Arguments = "--worker --max-jumps $Jumps --backend $Backend --save-debug --out-dir ""$Out\debug"" --log-dir ""$logs"""
    if ($Backend -eq 'simd') { $info.Arguments += ' --no-cuda --no-opencl' }
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $info.RedirectStandardOutput = $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $console = [IO.StreamWriter]::new((Join-Path $Out 'console.log'))
    $console.AutoFlush = $true
    $stderr = [IO.File]::Create((Join-Path $Out 'stderr.log'))
    $child = [Diagnostics.Process]::Start($info)
    $stderrCopy = $child.StandardError.BaseStream.CopyToAsync($stderr)
    $state.child_pid = $child.Id
    $state.state = 'running'
    Save-Status
    $nextPause = [long]$PauseEvery
    $stopping = $false
    $read = $child.StandardOutput.ReadLineAsync()
    while ($read -or !$child.HasExited) {
        if (Test-StopRequested) { $state.stop_reason = 'User stop requested.' }
        if ($state.stop_reason -and !$stopping) {
            $stopping = $true
            $state.state = 'stopping'
            Save-Status
            if (!$child.HasExited) {
                try { $child.StandardInput.WriteLine('stop') } finally { $child.StandardInput.Close() }
            }
        }
        if (!$read) { [void]$child.WaitForExit(50); continue }
        if (!$read.Wait(50)) { continue }
        $line = $read.GetAwaiter().GetResult()
        if ($null -eq $line) { $read = $null; continue }
        $console.WriteLine($line)
        $read = $child.StandardOutput.ReadLineAsync()
        if (!$line.StartsWith('@jumpbot ', [StringComparison]::Ordinal)) { continue }
        $s = $line.Substring(9) | ConvertFrom-Json -ErrorAction Stop
        if ($s.state -isnot [string] -or !$s.state) { throw 'Invalid worker snapshot.' }
        $state.progress = $s
        if (!$stopping) {
            if ($s.attempts -gt 0 -and
                (100L * ($s.attempts - $s.landed) -gt 2L * $Jumps -or
                 100L * $s.misses -gt 5L * $Jumps)) {
                $state.stop_reason = 'Rate threshold is unreachable.'
            } elseif ($s.state -in 'playing','dead' -and $s.jumps -ge $nextPause -and
                      $s.jumps -lt $Jumps -and !$child.HasExited) {
                # The pipe has no pause owner or conditional resume. Only a person can resume.
                $child.StandardInput.WriteLine('pause')
                $state.pause_requests++
                $nextPause = [long]$s.jumps + $PauseEvery
            }
        }
        Save-Status
    }
    $child.WaitForExit()
    $state.return_code = $child.ExitCode
    $final = $state.progress
    if (!(Test-Path -LiteralPath $logs)) { throw 'No worker CSV samples.' }
    $counts = Get-RecordedCounts
    $state.stats = $counts
    if ($stopping -or (Test-StopRequested)) { throw 'User or threshold stop.' }
    if ($child.ExitCode -ne 0 -or $final.state -cne 'stopped' -or $final.stop_reason -cne '达到最大跳跃次数') {
        throw "Worker did not finish the round: $($final.stop_reason)"
    }
    if ($final.paused -isnot [bool] -or $final.paused) { throw 'Final pause state does not allow another round.' }
    foreach ($key in $counts.Keys) {
        $n = $final.$key
        if (($n -isnot [long] -and $n -isnot [int]) -or $n -ne $counts[$key]) {
            throw "Final snapshot and worker CSV disagree: $key."
        }
    }
    if ($counts.attempts -ne $Jumps -or $final.jumps -ne $Jumps -or
        !$counts.judged -or 100L * $counts.landed -lt 98L * $counts.attempts -or
        100L * $counts.hits -lt 95L * $counts.judged) {
        throw 'Sample count or required landing/hit rate was not met.'
    }
    $state.round_pass = $true
    $state.stop_reason = $final.stop_reason
} catch {
    if (!$state.stop_reason) { $state.stop_reason = $_.Exception.Message }
} finally {
    if ($child) {
        if (!$child.HasExited) {
            # The native stdin reader requests cooperative stop on EOF, including on Ctrl+C.
            $child.StandardInput.Close()
            if ($read) { [void]$read.GetAwaiter().GetResult() }
            $child.StandardOutput.BaseStream.CopyTo([IO.Stream]::Null)
            $child.WaitForExit()
        }
        $state.return_code = $child.ExitCode
        if ($stderrCopy) { [void]$stderrCopy.GetAwaiter().GetResult() }
        $child.Dispose()
    }
    if ($console) { $console.Dispose() }
    if ($stderr) { $stderr.Dispose() }
    if (Test-StopRequested) { $state.round_pass = $false; $state.stop_reason = 'User stop requested.' }
    $state.state = if ($state.round_pass) { 'complete' } else { 'stopped' }
    if (!$state.stop_reason) { $state.stop_reason = 'Runner interrupted.' }
    Save-Status
    [IO.File]::WriteAllText((Join-Path $Out 'run-evidence.json'),
        (@{requested_jumps=$Jumps; backend=$Backend; return_code=$state.return_code
           final_status=$state.progress; pause_requests=$state.pause_requests
           frozen_exe_sha256=$exeHash; build_manifest_sha256=$buildHash} | ConvertTo-Json -Depth 10))
    $summary = $state | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText((Join-Path $Out 'summary.json'), $summary)
    Write-Output $summary
}
if (!$state.round_pass) { exit 1 }
exit 0
