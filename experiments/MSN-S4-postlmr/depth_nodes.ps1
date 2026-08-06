<#
.SYNOPSIS
    Fixed-depth node counts with UsePostLMR on vs off, over a tactical FEN set.

.DESCRIPTION
    Capture LMR is a REDUCTION, so the only comparison that can judge it is nodes at a
    FIXED DEPTH: at fixed time a reduction that saves nodes simply searches deeper and
    the node count says nothing. Bench (depth 7) is too shallow to be that measurement --
    a reduction that opens a ply later than the quiet one barely fires there -- so this
    script sweeps the depths where the engine actually plays.

    DRIVER: a real System.Diagnostics.Process with a live stdin writer and blocking
    StandardOutput.ReadLine() until `bestmove`. Redirecting stdin from a FILE delivers
    `quit` before the search finishes and truncates it (library/user-testing.md); the
    first version of this script did exactly that and reported 90 nodes at depth 14.
#>
[CmdletBinding()]
param(
    [string]$Engine = "target\release\manifold.exe",
    [int[]]$Depths = @(10, 12, 14),
    [string]$OutFile = "experiments\MSN-S4-postlmr\depth-nodes.txt"
)

$ErrorActionPreference = 'Stop'
$root = (Get-Location).Path
$enginePath = Join-Path $root $Engine

$positions = @(
    @{ name = 'startpos';   fen = 'startpos' },
    @{ name = 'kiwipete';   fen = 'fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1' },
    @{ name = 'italian';    fen = 'fen r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10' },
    @{ name = 'promo-race'; fen = 'fen rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8' },
    @{ name = 'chaos';      fen = 'fen r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1' },
    @{ name = 'sicilian';   fen = 'fen 2kr3r/pp1q1ppp/5n2/1Nb5/2Pp1B2/7Q/P4PPP/1R3RK1 w - - 0 1' }
)

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

function Invoke-Search {
    param([bool]$Enabled, [string]$Position, [int]$Depth)

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName               = $enginePath
    $psi.WorkingDirectory       = $root
    $psi.RedirectStandardInput  = $true
    $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute        = $false
    $p = [System.Diagnostics.Process]::Start($psi)

    $w = $p.StandardInput
    $w.WriteLine('uci')
    Read-Until $p 'uciok' | Out-Null
    $w.WriteLine("setoption name UsePostLMR value $($Enabled.ToString().ToLower())")
    $w.WriteLine('setoption name Hash value 64')
    $w.WriteLine('ucinewgame')
    $w.WriteLine('isready')
    Read-Until $p 'readyok' | Out-Null
    $w.WriteLine("position $Position")
    $w.WriteLine("go depth $Depth")
    $out = Read-Until $p '^bestmove'
    $w.WriteLine('quit')
    $p.WaitForExit(10000) | Out-Null

    $last = $out | Where-Object { $_ -match "^info depth $Depth " } | Select-Object -Last 1
    $best = $out | Where-Object { $_ -match '^bestmove ' } | Select-Object -Last 1
    $nodes = 0
    if ($last -match ' nodes (\d+)') { $nodes = [int64]$matches[1] }
    $move = ''
    if ($best -match '^bestmove (\S+)') { $move = $matches[1] }
    [pscustomobject]@{ Nodes = $nodes; Move = $move }
}

$lines = @()
$lines += "Post-LMR fixed-depth node counts -- $(Get-Date -Format s)"
$lines += "Engine: $enginePath"
$lines += ''

foreach ($depth in $Depths) {
    $onTotal = 0L; $offTotal = 0L; $disagree = @()
    $lines += "== depth $depth =="
    $lines += ('{0,-12} {1,12} {2,12} {3,9}  {4}' -f 'position', 'on', 'off', 'delta%', 'bestmove')
    foreach ($pos in $positions) {
        $on  = Invoke-Search -Enabled $true  -Position $pos.fen -Depth $depth
        $off = Invoke-Search -Enabled $false -Position $pos.fen -Depth $depth
        $onTotal += $on.Nodes; $offTotal += $off.Nodes
        $delta = if ($off.Nodes -gt 0) { 100.0 * ($on.Nodes - $off.Nodes) / $off.Nodes } else { 0 }
        $agree = if ($on.Move -eq $off.Move) { $on.Move } else { "$($on.Move) != $($off.Move)" }
        if ($on.Move -ne $off.Move) { $disagree += $pos.name }
        $lines += ('{0,-12} {1,12} {2,12} {3,9:N2}  {4}' -f $pos.name, $on.Nodes, $off.Nodes, $delta, $agree)
    }
    $total = if ($offTotal -gt 0) { 100.0 * ($onTotal - $offTotal) / $offTotal } else { 0 }
    $lines += ('{0,-12} {1,12} {2,12} {3,9:N2}  disagreements: {4}' -f 'TOTAL', $onTotal, $offTotal, $total, ($disagree.Count))
    if ($disagree.Count -gt 0) { $lines += "  differing: $($disagree -join ', ')" }
    $lines += ''
}

$lines | ForEach-Object { Write-Output $_ }
$lines | Set-Content -LiteralPath $OutFile -Encoding utf8
Write-Output "wrote $OutFile"

