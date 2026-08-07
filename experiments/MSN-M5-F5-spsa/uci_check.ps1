# Manual UCI verification that the M5-F5 tuned values are the SHIPPED defaults.
#
# Two things a bench signature cannot show: that the handshake advertises the tuned
# numbers as its defaults (a GUI echoing them back must be a no-op), and that a real
# time-managed `go movetime` search still behaves. Process-based driver per mission
# AGENTS.md rule 7 -- a piped here-string closes stdin and aborts `go movetime`.
$ErrorActionPreference = 'Stop'
$root   = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'
$net    = Join-Path $root 'nets\main.nnue'

$expected = @{
    LmrCoefficient         = 2754
    LmrBase                = 996
    LmrTtPvReduction       = 1028
    LmrHistoryNumerator    = 459
    RfpMarginPerDepth      = 95
    RfpTtPvMargin          = 22
    FutilityBaseMargin     = 125
    FutilityMarginPerDepth = 106
}

$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = $engine
$info.WorkingDirectory = $root
$info.RedirectStandardInput = $true
$info.RedirectStandardOutput = $true
$info.UseShellExecute = $false
$process = [System.Diagnostics.Process]::Start($info)
$send = {
    param($line)
    $process.StandardInput.WriteLine($line)
    $process.StandardInput.Flush()
}

& $send "setoption name EvalFile value $net"
& $send 'uci'
$options = @{}
while ($true) {
    $line = $process.StandardOutput.ReadLine()
    if ($null -eq $line) { throw 'engine closed stdout before uciok' }
    if ($line -match '^option name (\S+) type spin default (-?\d+)') {
        $options[$Matches[1]] = [int]$Matches[2]
    }
    if ($line -eq 'uciok') { break }
}

Write-Output '--- handshake defaults ---'
$allMatched = $true
foreach ($name in $expected.Keys | Sort-Object) {
    $actual = $options[$name]
    $ok = ($actual -eq $expected[$name])
    if (-not $ok) { $allMatched = $false }
    Write-Output ("{0,-24} advertised {1,6}   expected {2,6}   {3}" -f $name, $actual, $expected[$name], $(if ($ok) { 'OK' } else { 'MISMATCH' }))
}
Write-Output ("handshake verdict: {0}" -f $(if ($allMatched) { 'all tuned values are the advertised defaults' } else { 'MISMATCH' }))

Write-Output ''
Write-Output '--- time-managed search (go movetime 3000, startpos) ---'
& $send 'ucinewgame'
& $send 'position startpos'
& $send 'go movetime 3000'
$last = ''
while ($true) {
    $line = $process.StandardOutput.ReadLine()
    if ($null -eq $line) { throw 'engine closed stdout before bestmove' }
    if ($line -match '^info depth') { $last = $line }
    if ($line -match '^bestmove') { Write-Output $last; Write-Output $line; break }
}

Write-Output ''
Write-Output '--- echoing the advertised defaults back must not change the tree ---'
& $send 'ucinewgame'
& $send 'position startpos'
& $send 'go depth 12'
$before = 0
while ($true) {
    $line = $process.StandardOutput.ReadLine()
    if ($line -match '^info .*\bnodes (\d+)\b') { $before = [uint64]$Matches[1] }
    if ($line -match '^bestmove') { break }
}
foreach ($name in $expected.Keys) { & $send "setoption name $name value $($expected[$name])" }
& $send 'ucinewgame'
& $send 'position startpos'
& $send 'go depth 12'
$after = 0
while ($true) {
    $line = $process.StandardOutput.ReadLine()
    if ($line -match '^info .*\bnodes (\d+)\b') { $after = [uint64]$Matches[1] }
    if ($line -match '^bestmove') { break }
}
Write-Output ("depth-12 nodes before {0}, after echoing defaults {1} -- {2}" -f $before, $after, $(if ($before -eq $after) { 'identical (no-op, as required)' } else { 'DIFFERENT' }))

& $send 'quit'
$process.WaitForExit(30000) | Out-Null
if (-not $process.HasExited) { $process.Kill() }
