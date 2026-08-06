# Why does capture LMR pick d7c8r instead of d7c8q on the promotion-race position?
#
# An underpromotion is either a real tactic or a symptom of the reduction mis-scoring a
# promotion capture. This prints the full PV and score of both arms so the question is
# settled by the score, not by which move looks more natural.
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'

function Read-Until([System.Diagnostics.Process]$p, [string]$sentinel) {
    $lines = @()
    while ($true) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        $lines += $line
        if ($line -match $sentinel) { break }
    }
    $lines
}

function Probe([string]$label, [string[]]$setup, [string]$position, [string]$go) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $engine; $psi.WorkingDirectory = $root
    $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute = $false
    $p = [System.Diagnostics.Process]::Start($psi)
    $w = $p.StandardInput
    $w.WriteLine('uci'); Read-Until $p 'uciok' | Out-Null
    foreach ($line in $setup) { $w.WriteLine($line) }
    $w.WriteLine('setoption name Hash value 64')
    $w.WriteLine('ucinewgame'); $w.WriteLine('isready'); Read-Until $p 'readyok' | Out-Null
    $w.WriteLine("position $position")
    $w.WriteLine($go)
    $out = Read-Until $p '^bestmove'
    $w.WriteLine('quit'); $p.WaitForExit(10000) | Out-Null

    Write-Output "===== $label ====="
    $out | Where-Object { $_ -match 'multipv 1' -or $_ -match '^bestmove' } |
        ForEach-Object { Write-Output $_ }
    Write-Output ''
}

$promo = 'fen rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8'
$start = 'startpos'

Probe 'promo-race, capture LMR ON'  @('setoption name UseCaptureLMR value true')  $promo 'go depth 14'
Probe 'promo-race, capture LMR OFF' @('setoption name UseCaptureLMR value false') $promo 'go depth 14'
Probe 'startpos, capture LMR ON'    @('setoption name UseCaptureLMR value true')  $start 'go depth 14'
Probe 'startpos, capture LMR OFF'   @('setoption name UseCaptureLMR value false') $start 'go depth 14'
